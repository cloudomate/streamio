//! KubeVirt VM provisioner.
//!
//! Creates, starts, stops, and deletes per-user VMs via the Kubernetes API.
//! Uses KubeVirt CRDs (`VirtualMachine`) and CDI `DataVolume` for persistent
//! per-user disks cloned from a shared base image PVC.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use kube::{
    api::{Api, DynamicObject, GroupVersionResource, Patch, PatchParams, PostParams},
    Client, ResourceExt,
};
use serde_json::json;
use streamio_types::ProvisionRequest;
use tracing::{info, warn};
use uuid::Uuid;

// ── GroupVersionResource constants ───────────────────────────────────────────

fn kubevirt_vm_gvr() -> GroupVersionResource {
    GroupVersionResource::gvr("kubevirt.io", "v1", "virtualmachines")
}

fn kubevirt_vmi_gvr() -> GroupVersionResource {
    GroupVersionResource::gvr("kubevirt.io", "v1", "virtualmachineinstances")
}

fn cdi_datavolume_gvr() -> GroupVersionResource {
    GroupVersionResource::gvr("cdi.kubevirt.io", "v1beta1", "datavolumes")
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Handle returned after a successful provision call.
pub struct VmHandle {
    pub vm_name: String,
    pub ns: String,
    pub disk_pvc: String,
}

/// Trait abstracting VM lifecycle operations. Allows mocking in tests.
#[async_trait]
pub trait VmProvisioner: Send + Sync {
    /// Create DataVolume + VirtualMachine for a user. VM starts in stopped state.
    async fn provision(&self, req: &ProvisionRequest, backend_id: Uuid) -> Result<VmHandle>;
    /// Power on a stopped VM.
    async fn start(&self, vm_name: &str, ns: &str) -> Result<()>;
    /// Gracefully power off a running VM.
    async fn stop(&self, vm_name: &str, ns: &str) -> Result<()>;
    /// Delete VM, DataVolume, and associated PVC.
    async fn delete(&self, vm_name: &str, ns: &str, disk_pvc: &str) -> Result<()>;
    /// Return the VM's printable power state (lowercase): stopped, starting, running, stopping, provisioning.
    async fn state(&self, vm_name: &str, ns: &str) -> Result<String>;
}

// ── KubeVirt implementation ──────────────────────────────────────────────────

pub struct KubeVirtProvisioner {
    client: Client,
    default_ns: String,
    gateway_url: String,
    jwt_secret: String,
}

impl KubeVirtProvisioner {
    /// Build a provisioner. Uses in-cluster service account when running in K8s,
    /// falls back to local kubeconfig for development.
    pub async fn new(ns: String, gateway_url: String, jwt_secret: String) -> Result<Self> {
        let client = Client::try_default()
            .await
            .context("Failed to connect to Kubernetes API — is KUBECONFIG set or running in-cluster?")?;
        info!("KubeVirt provisioner connected to Kubernetes (namespace: {ns})");
        Ok(Self { client, default_ns: ns, gateway_url, jwt_secret })
    }

    /// Sanitize a user subject into a valid Kubernetes name.
    /// Rules: lowercase, max 60 chars, non-alphanumeric → '-', leading/trailing '-' trimmed.
    fn vm_name(user_sub: &str) -> String {
        let sanitized: String = format!("vdi-{}", user_sub)
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
            .collect();
        // Collapse consecutive dashes, trim leading/trailing dashes
        let collapsed = sanitized
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        // Kubernetes names max 253 chars; keep short for readability
        collapsed.chars().take(60).collect()
    }

    fn vm_api(&self, ns: &str) -> Api<DynamicObject> {
        Api::namespaced_with(self.client.clone(), ns, &kubevirt_vm_gvr())
    }

    fn vmi_api(&self, ns: &str) -> Api<DynamicObject> {
        Api::namespaced_with(self.client.clone(), ns, &kubevirt_vmi_gvr())
    }

    fn dv_api(&self, ns: &str) -> Api<DynamicObject> {
        Api::namespaced_with(self.client.clone(), ns, &cdi_datavolume_gvr())
    }

    /// Build the cloud-init userData script injected into Linux VMs.
    fn cloud_init_userdata(&self, backend_id: Uuid) -> String {
        format!(
            r#"#!/bin/bash
set -e
# Write streamio configuration
mkdir -p /etc
cat > /etc/streamio.env <<'ENVEOF'
PORT=9001
GATEWAY_URL={gateway_url}
BACKEND_ID={backend_id}
BACKEND_TOKEN_SECRET={jwt_secret}
RUST_LOG=info
ENVEOF
# Install streamio if not already present (base image may pre-install it)
if ! command -v streamio &>/dev/null; then
  ARCH=$(uname -m)
  URL="https://github.com/streamio/streamio/releases/latest/download/streamio-linux-${{ARCH}}.tar.gz"
  curl -fsSL "$URL" | tar xz -C /usr/local/bin
fi
# Install and start systemd service
cat > /etc/systemd/system/streamio.service <<'SVCEOF'
[Unit]
Description=Streamio VDI Agent
After=network-online.target graphical-session.target
Wants=network-online.target
[Service]
EnvironmentFile=/etc/streamio.env
ExecStart=/usr/local/bin/streamio
Restart=on-failure
RestartSec=5s
[Install]
WantedBy=multi-user.target
SVCEOF
systemctl daemon-reload
systemctl enable --now streamio
"#,
            gateway_url = self.gateway_url,
            backend_id = backend_id,
            jwt_secret = self.jwt_secret,
        )
    }
}

#[async_trait]
impl VmProvisioner for KubeVirtProvisioner {
    async fn provision(&self, req: &ProvisionRequest, backend_id: Uuid) -> Result<VmHandle> {
        let ns = &self.default_ns;
        let vm_name = Self::vm_name(&req.user_sub);
        let disk_pvc = format!("{}-disk", vm_name);

        info!("Provisioning VM: name={vm_name} ns={ns} base={} size={}", req.base_pvc, req.disk_size);

        // 1. Create DataVolume (CDI clones base_pvc → user disk)
        let dv_api = self.dv_api(ns);
        let dv = DynamicObject::new(&disk_pvc, &cdi_datavolume_gvr())
            .within(ns);
        let mut dv = dv;
        dv.data = json!({
            "apiVersion": "cdi.kubevirt.io/v1beta1",
            "kind": "DataVolume",
            "metadata": {
                "name": disk_pvc,
                "namespace": ns,
                "labels": { "streamio/user": req.user_sub, "streamio/backend-id": backend_id.to_string() }
            },
            "spec": {
                "source": {
                    "pvc": { "namespace": ns, "name": req.base_pvc }
                },
                "pvc": {
                    "accessModes": ["ReadWriteOnce"],
                    "resources": { "requests": { "storage": req.disk_size } }
                }
            }
        });

        dv_api
            .create(&PostParams::default(), &dv)
            .await
            .context("Failed to create DataVolume")?;
        info!("DataVolume created: {disk_pvc}");

        // 2. Create VirtualMachine (initially stopped)
        let userdata = self.cloud_init_userdata(backend_id);
        let vm_api = self.vm_api(ns);
        let mut vm = DynamicObject::new(&vm_name, &kubevirt_vm_gvr())
            .within(ns);
        vm.data = json!({
            "apiVersion": "kubevirt.io/v1",
            "kind": "VirtualMachine",
            "metadata": {
                "name": vm_name,
                "namespace": ns,
                "labels": {
                    "streamio/user": req.user_sub,
                    "streamio/backend-id": backend_id.to_string()
                }
            },
            "spec": {
                "running": false,
                "template": {
                    "metadata": {
                        "labels": { "streamio/vm": vm_name }
                    },
                    "spec": {
                        "domain": {
                            "devices": {
                                "disks": [
                                    { "name": "rootdisk", "disk": { "bus": "virtio" } },
                                    { "name": "cloudinit", "disk": { "bus": "virtio" } }
                                ],
                                "interfaces": [{ "name": "default", "masquerade": {} }]
                            },
                            "resources": {
                                "requests": { "memory": req.memory }
                            },
                            "cpu": { "cores": req.cpu_cores }
                        },
                        "networks": [{ "name": "default", "pod": {} }],
                        "volumes": [
                            {
                                "name": "rootdisk",
                                "dataVolume": { "name": disk_pvc }
                            },
                            {
                                "name": "cloudinit",
                                "cloudInitNoCloud": { "userData": userdata }
                            }
                        ]
                    }
                }
            }
        });

        vm_api
            .create(&PostParams::default(), &vm)
            .await
            .context("Failed to create VirtualMachine")?;
        info!("VirtualMachine created: {vm_name} (stopped)");

        Ok(VmHandle { vm_name, ns: ns.clone(), disk_pvc })
    }

    async fn start(&self, vm_name: &str, ns: &str) -> Result<()> {
        let vm_api = self.vm_api(ns);
        vm_api
            .patch(
                vm_name,
                &PatchParams::apply("streamio-gateway"),
                &Patch::Merge(json!({ "spec": { "running": true } })),
            )
            .await
            .context("Failed to start VM")?;
        info!("VM started: {vm_name}");
        Ok(())
    }

    async fn stop(&self, vm_name: &str, ns: &str) -> Result<()> {
        let vm_api = self.vm_api(ns);
        vm_api
            .patch(
                vm_name,
                &PatchParams::apply("streamio-gateway"),
                &Patch::Merge(json!({ "spec": { "running": false } })),
            )
            .await
            .context("Failed to stop VM")?;
        info!("VM stopped: {vm_name}");
        Ok(())
    }

    async fn delete(&self, vm_name: &str, ns: &str, disk_pvc: &str) -> Result<()> {
        let vm_api = self.vm_api(ns);
        let dv_api = self.dv_api(ns);

        // Delete VM first (stops it if running)
        match vm_api.delete(vm_name, &Default::default()).await {
            Ok(_) => info!("VirtualMachine deleted: {vm_name}"),
            Err(e) => warn!("VM deletion warning (may already be gone): {e}"),
        }

        // Delete DataVolume (CDI also deletes the underlying PVC)
        match dv_api.delete(disk_pvc, &Default::default()).await {
            Ok(_) => info!("DataVolume deleted: {disk_pvc}"),
            Err(e) => warn!("DataVolume deletion warning: {e}"),
        }

        Ok(())
    }

    async fn state(&self, vm_name: &str, ns: &str) -> Result<String> {
        let vm_api = self.vm_api(ns);
        let vm = vm_api
            .get(vm_name)
            .await
            .context("Failed to get VM state")?;

        let state = vm
            .data
            .get("status")
            .and_then(|s| s.get("printableStatus"))
            .and_then(|s| s.as_str())
            .unwrap_or("Unknown")
            .to_lowercase();

        Ok(state)
    }
}

// ── Default VM spec (from Config) ────────────────────────────────────────────

/// Default VM specification read from environment variables.
/// Used when auto-provisioning on first user login.
#[derive(Debug, Clone)]
pub struct DefaultVmSpec {
    pub os_type: streamio_types::OsType,
    pub base_pvc: String,
    pub disk_size: String,
    pub memory: String,
    pub cpu_cores: u32,
}

impl DefaultVmSpec {
    pub fn from_env() -> Option<Self> {
        Some(DefaultVmSpec {
            os_type: streamio_types::OsType::Ubuntu,
            base_pvc: std::env::var("DEFAULT_BASE_PVC").ok()?,
            disk_size: std::env::var("DEFAULT_DISK_SIZE")
                .unwrap_or_else(|_| "60Gi".into()),
            memory: std::env::var("DEFAULT_VM_MEMORY")
                .unwrap_or_else(|_| "4Gi".into()),
            cpu_cores: std::env::var("DEFAULT_VM_CPU")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(2),
        })
    }

    pub fn into_provision_request(&self, user_sub: String, label: Option<String>) -> ProvisionRequest {
        ProvisionRequest {
            user_sub,
            os_type: self.os_type.clone(),
            base_pvc: self.base_pvc.clone(),
            disk_size: self.disk_size.clone(),
            memory: self.memory.clone(),
            cpu_cores: self.cpu_cores,
            label,
        }
    }
}
