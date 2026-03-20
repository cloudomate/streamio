/*
 * Streamio Virtual Display Driver (IddCx UMDF2)
 *
 * Creates virtual monitors on demand via IOCTL. Each monitor appears as a
 * real display in Windows — GStreamer's d3d11screencapturesrc can capture it
 * independently by monitor-index.
 *
 * Based on Microsoft IddCx sample and Virtual-Display-Driver project.
 *
 * Key architecture:
 *   - UMDF2 driver (user-mode, no BSOD risk)
 *   - IddCx framework handles display adapter + monitor lifecycle
 *   - User-mode control via DeviceIoControl (IOCTL_DISPLAY_CREATE/DESTROY/LIST)
 *   - Swapchain: IddCx provides frames, we release them immediately
 *     (capture is done by GStreamer, not by this driver)
 */

#include <windows.h>
#include <wdf.h>

/* IddCx headers */
#include <IddCx.h>

#include "streamio-display.h"

/* ── Forward declarations ─────────────────────────────────────────── */

extern "C" DRIVER_INITIALIZE DriverEntry;
EVT_WDF_DRIVER_DEVICE_ADD  EvtDeviceAdd;
EVT_WDF_DEVICE_D0_ENTRY    EvtDeviceD0Entry;

EVT_IDD_CX_ADAPTER_INIT_FINISHED     EvtAdapterInitFinished;
EVT_IDD_CX_ADAPTER_COMMIT_MODES      EvtAdapterCommitModes;
EVT_IDD_CX_MONITOR_GET_DEFAULT_DESCRIPTION_MODES EvtMonitorGetDefaultModes;
EVT_IDD_CX_MONITOR_QUERY_TARGET_MODES EvtMonitorQueryTargetModes;
EVT_IDD_CX_PARSE_MONITOR_DESCRIPTION  EvtParseMonitorDescription;
EVT_IDD_CX_MONITOR_ASSIGN_SWAPCHAIN   EvtMonitorAssignSwapChain;
EVT_IDD_CX_MONITOR_UNASSIGN_SWAPCHAIN EvtMonitorUnassignSwapChain;

/* ── Per-monitor context ──────────────────────────────────────────── */

typedef struct _MONITOR_CONTEXT {
    UINT32 display_id;
    UINT32 width;
    UINT32 height;
    UINT32 refresh_hz;
    BOOLEAN active;
    IDDCX_MONITOR monitor_object;
    HANDLE swapchain_thread;
    volatile BOOLEAN stop_thread;
} MONITOR_CONTEXT;

/* ── Per-adapter (device) context ─────────────────────────────────── */

typedef struct _DEVICE_CONTEXT {
    IDDCX_ADAPTER adapter;
    WDFDEVICE device;
    MONITOR_CONTEXT monitors[STREAMIO_MAX_DISPLAYS];
    UINT32 monitor_count;
    WDFWAITLOCK lock;
} DEVICE_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(DEVICE_CONTEXT, GetDeviceContext)

/* ── EDID generation ──────────────────────────────────────────────── */

/*
 * Generate a minimal EDID block (128 bytes) for a virtual display.
 * This tells Windows the monitor's preferred resolution/refresh rate.
 */
static void GenerateEdid(BYTE edid[128], UINT32 width, UINT32 height, UINT32 refresh_hz, UINT32 display_id)
{
    memset(edid, 0, 128);

    /* Header */
    edid[0] = 0x00; edid[1] = 0xFF; edid[2] = 0xFF; edid[3] = 0xFF;
    edid[4] = 0xFF; edid[5] = 0xFF; edid[6] = 0xFF; edid[7] = 0x00;

    /* Manufacturer: "STR" (Streamio) - encoded as 3x5-bit chars */
    /* S=19(0x13), T=20(0x14), R=18(0x12) */
    edid[8] = (BYTE)(((0x13 - 1) << 2) | ((0x14 - 1) >> 3));
    edid[9] = (BYTE)(((0x14 - 1) << 5) | (0x12 - 1));

    /* Product code (unique per display) */
    edid[10] = (BYTE)(0x01 + display_id);
    edid[11] = 0x00;

    /* Serial number */
    edid[12] = 0x01; edid[13] = 0x00; edid[14] = 0x00; edid[15] = 0x00;

    /* Week 1, Year 2024 (offset from 1990) */
    edid[16] = 1;
    edid[17] = 34;

    /* EDID version 1.3 */
    edid[18] = 1;
    edid[19] = 3;

    /* Digital input, 8 bits per color */
    edid[20] = 0x80;

    /* Screen size: cm (approximate) */
    edid[21] = (BYTE)(width * 53 / 1920);   /* ~53cm for 1920 width = ~24" */
    edid[22] = (BYTE)(height * 30 / 1080);

    /* Gamma 2.2 */
    edid[23] = 120;

    /* Supported features */
    edid[24] = 0x0A;

    /* Chromaticity coordinates (sRGB standard) */
    edid[25] = 0xEE; edid[26] = 0x91; edid[27] = 0xA3; edid[28] = 0x54;
    edid[29] = 0x4C; edid[30] = 0x99; edid[31] = 0x26; edid[32] = 0x0F;
    edid[33] = 0x50; edid[34] = 0x54;

    /* Standard timings: unused */
    for (int i = 38; i < 54; i += 2) {
        edid[i] = 0x01;
        edid[i + 1] = 0x01;
    }

    /* Detailed Timing Descriptor #1 — preferred mode */
    UINT32 pixel_clock = (UINT32)((UINT64)width * height * refresh_hz / 10000);
    edid[54] = (BYTE)(pixel_clock & 0xFF);
    edid[55] = (BYTE)((pixel_clock >> 8) & 0xFF);

    UINT32 hblank = width * 20 / 100;  /* ~20% blanking */
    UINT32 vblank = height * 5 / 100;

    edid[56] = (BYTE)(width & 0xFF);
    edid[57] = (BYTE)(hblank & 0xFF);
    edid[58] = (BYTE)(((width >> 8) << 4) | ((hblank >> 8) & 0x0F));

    edid[59] = (BYTE)(height & 0xFF);
    edid[60] = (BYTE)(vblank & 0xFF);
    edid[61] = (BYTE)(((height >> 8) << 4) | ((vblank >> 8) & 0x0F));

    /* Hsync/Vsync offsets and widths */
    edid[62] = 48;  /* h front porch */
    edid[63] = 32;  /* h sync width */
    edid[64] = 0x35; /* v front porch + sync width */
    edid[65] = 0x00;

    /* Image size (mm) */
    UINT32 h_mm = width * 530 / 1920;
    UINT32 v_mm = height * 300 / 1080;
    edid[66] = (BYTE)(h_mm & 0xFF);
    edid[67] = (BYTE)(v_mm & 0xFF);
    edid[68] = (BYTE)(((h_mm >> 8) << 4) | ((v_mm >> 8) & 0x0F));

    /* No border */
    edid[69] = 0;
    edid[70] = 0;

    /* Signal: non-interlaced, digital */
    edid[71] = 0x18;

    /* Descriptor #2: Monitor name "Streamio N" */
    edid[72] = 0; edid[73] = 0; edid[74] = 0; edid[75] = 0xFC; edid[76] = 0;
    char name[14];
    int name_len = sprintf_s(name, sizeof(name), "Streamio %u", display_id);
    for (int i = 0; i < 13; i++) {
        edid[77 + i] = (i < name_len) ? (BYTE)name[i] : 0x0A;
    }

    /* Descriptor #3: Monitor range limits */
    edid[90] = 0; edid[91] = 0; edid[92] = 0; edid[93] = 0xFD; edid[94] = 0;
    edid[95] = (BYTE)(refresh_hz - 1);  /* min V rate */
    edid[96] = (BYTE)(refresh_hz + 1);  /* max V rate */
    edid[97] = 30;   /* min H rate kHz */
    edid[98] = 150;  /* max H rate kHz */
    edid[99] = 25;   /* max pixel clock / 10 MHz */
    edid[100] = 0x00; /* no GTF */

    /* Descriptor #4: Serial number string */
    edid[108] = 0; edid[109] = 0; edid[110] = 0; edid[111] = 0xFF; edid[112] = 0;
    char serial[14];
    int serial_len = sprintf_s(serial, sizeof(serial), "STRVD%07u", display_id);
    for (int i = 0; i < 13; i++) {
        edid[113 + i] = (i < serial_len) ? (BYTE)serial[i] : 0x0A;
    }

    /* Extension count = 0 */
    edid[126] = 0;

    /* Checksum */
    BYTE sum = 0;
    for (int i = 0; i < 127; i++) sum += edid[i];
    edid[127] = (BYTE)(256 - sum);
}

/* ── Swapchain processing thread ──────────────────────────────────── */

/*
 * IddCx provides a swapchain when the OS renders to our virtual display.
 * We must acquire and release frames — otherwise the OS stalls.
 * We don't need the frame data (GStreamer captures via DXGI independently).
 */
static DWORD WINAPI SwapchainThread(LPVOID param)
{
    MONITOR_CONTEXT* ctx = (MONITOR_CONTEXT*)param;
    IDARG_IN_SWAPCHAINSETDEVICE set_device = {};

    /* We need a D3D device to process the swapchain */
    /* For now, create a minimal D3D11 device */
    ID3D11Device* d3d_device = nullptr;
    D3D_FEATURE_LEVEL feature_level;
    HRESULT hr = D3D11CreateDevice(
        nullptr, D3D_DRIVER_TYPE_HARDWARE, nullptr,
        0, nullptr, 0, D3D11_SDK_VERSION,
        &d3d_device, &feature_level, nullptr
    );

    if (FAILED(hr)) {
        /* Try WARP (software) if no hardware */
        hr = D3D11CreateDevice(
            nullptr, D3D_DRIVER_TYPE_WARP, nullptr,
            0, nullptr, 0, D3D11_SDK_VERSION,
            &d3d_device, &feature_level, nullptr
        );
    }

    if (FAILED(hr) || !d3d_device) {
        return 1;
    }

    /* Tell IddCx which D3D device to use for this swapchain */
    set_device.pDevice = (IUnknown*)d3d_device;
    /* Note: IddCxSwapChainSetDevice is called within the assign callback context,
       not here. The thread just processes frames. */

    while (!ctx->stop_thread) {
        /* Acquire and immediately release frames to keep the pipeline flowing */
        IDXGIResource* resource = nullptr;
        IDARG_OUT_RELEASEANDACQUIREBUFFER buf_out = {};
        IDARG_IN_RELEASEANDACQUIREBUFFER buf_in = {};
        buf_in.Reason = IDDCX_UPDATE_REASON_OTHER;

        /* This blocks until a frame is available or timeout */
        NTSTATUS status = IddCxSwapChainReleaseAndAcquireBuffer(
            ctx->monitor_object, &buf_in, &buf_out);

        if (NT_SUCCESS(status)) {
            /* Release immediately — we don't process frames */
            IddCxSwapChainFinishedProcessingFrame(ctx->monitor_object);
        } else {
            /* No frame available or error — sleep briefly */
            Sleep(16);  /* ~60fps timing */
        }
    }

    if (d3d_device) {
        d3d_device->Release();
    }

    return 0;
}

/* ── IddCx Callbacks ─────────────────────────────────────────────── */

NTSTATUS EvtAdapterInitFinished(IDDCX_ADAPTER adapter, const IDARG_IN_ADAPTER_INIT_FINISHED* args)
{
    if (!NT_SUCCESS(args->AdapterInitStatus)) {
        return args->AdapterInitStatus;
    }
    return STATUS_SUCCESS;
}

NTSTATUS EvtAdapterCommitModes(IDDCX_ADAPTER adapter, const IDARG_IN_COMMITMODES2* args)
{
    /* OS is committing display modes — acknowledge */
    UNREFERENCED_PARAMETER(adapter);
    UNREFERENCED_PARAMETER(args);
    return STATUS_SUCCESS;
}

NTSTATUS EvtParseMonitorDescription(
    const IDARG_IN_PARSEMONITORDESCRIPTION* in_args,
    IDARG_OUT_PARSEMONITORDESCRIPTION* out_args)
{
    /* We provide EDID — IddCx will parse it for us if we return NOT_HANDLED,
       or we can parse it ourselves. Let IddCx handle it. */
    UNREFERENCED_PARAMETER(in_args);
    out_args->MonitorDescription.Size = 0;
    return STATUS_SUCCESS;
}

NTSTATUS EvtMonitorGetDefaultModes(
    IDDCX_MONITOR monitor,
    const IDARG_IN_GETDEFAULTDESCRIPTIONMODES* in_args,
    IDARG_OUT_GETDEFAULTDESCRIPTIONMODES* out_args)
{
    UNREFERENCED_PARAMETER(monitor);
    UNREFERENCED_PARAMETER(in_args);
    /* Return 0 — we use target modes instead */
    out_args->DefaultMonitorModeBufferOutputCount = 0;
    return STATUS_SUCCESS;
}

NTSTATUS EvtMonitorQueryTargetModes(
    IDDCX_MONITOR monitor,
    const IDARG_IN_QUERYTARGETMODES* in_args,
    IDARG_OUT_QUERYTARGETMODES* out_args)
{
    /* Find our monitor context */
    /* For simplicity, report one target mode matching our configured resolution */

    /* TODO: look up context from monitor handle to get width/height/refresh */
    IDDCX_TARGET_MODE mode = {};
    mode.Size = sizeof(mode);
    mode.TargetVideoSignalInfo.totalSize.cx = 1920;
    mode.TargetVideoSignalInfo.totalSize.cy = 1080;
    mode.TargetVideoSignalInfo.activeSize.cx = 1920;
    mode.TargetVideoSignalInfo.activeSize.cy = 1080;
    mode.TargetVideoSignalInfo.vSyncFreq.Numerator = 60;
    mode.TargetVideoSignalInfo.vSyncFreq.Denominator = 1;
    mode.TargetVideoSignalInfo.hSyncFreq.Numerator = 67500;
    mode.TargetVideoSignalInfo.hSyncFreq.Denominator = 1;
    mode.TargetVideoSignalInfo.scanLineOrdering = IDDCX_MONITOR_MODE_ORIGIN_DRIVER;
    mode.TargetVideoSignalInfo.pixelRate = 148500000;
    mode.TargetVideoSignalInfo.AdditionalSignalInfo.vSyncFreqDivider = 1;

    if (in_args->TargetModeBufferInputCount >= 1) {
        in_args->pTargetModes[0] = mode;
        out_args->TargetModeBufferOutputCount = 1;
    } else {
        out_args->TargetModeBufferOutputCount = 1;
    }

    return STATUS_SUCCESS;
}

NTSTATUS EvtMonitorAssignSwapChain(
    IDDCX_MONITOR monitor,
    const IDARG_IN_SETSWAPCHAIN* args)
{
    /* OS assigned a swapchain — start processing thread */
    /* TODO: find monitor context and start SwapchainThread */
    UNREFERENCED_PARAMETER(monitor);
    UNREFERENCED_PARAMETER(args);
    return STATUS_SUCCESS;
}

NTSTATUS EvtMonitorUnassignSwapChain(IDDCX_MONITOR monitor)
{
    /* OS unassigned swapchain — stop processing thread */
    UNREFERENCED_PARAMETER(monitor);
    return STATUS_SUCCESS;
}

/* ── IOCTL handler — create/destroy virtual displays ──────────────── */

EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL EvtIoDeviceControl;

void EvtIoDeviceControl(
    WDFQUEUE queue,
    WDFREQUEST request,
    size_t output_length,
    size_t input_length,
    ULONG ioctl_code)
{
    UNREFERENCED_PARAMETER(output_length);
    UNREFERENCED_PARAMETER(input_length);

    WDFDEVICE device = WdfIoQueueGetDevice(queue);
    DEVICE_CONTEXT* ctx = GetDeviceContext(device);
    NTSTATUS status = STATUS_INVALID_PARAMETER;

    WdfWaitLockAcquire(ctx->lock, NULL);

    switch (ioctl_code) {
    case IOCTL_DISPLAY_CREATE: {
        STREAMIO_DISPLAY_CREATE_REQUEST* req = nullptr;
        STREAMIO_DISPLAY_CREATE_RESPONSE* resp = nullptr;

        status = WdfRequestRetrieveInputBuffer(request, sizeof(*req), (PVOID*)&req, NULL);
        if (!NT_SUCCESS(status)) break;

        status = WdfRequestRetrieveOutputBuffer(request, sizeof(*resp), (PVOID*)&resp, NULL);
        if (!NT_SUCCESS(status)) break;

        if (ctx->monitor_count >= STREAMIO_MAX_DISPLAYS) {
            status = STATUS_INSUFFICIENT_RESOURCES;
            break;
        }

        /* Find a free slot */
        UINT32 slot = STREAMIO_MAX_DISPLAYS;
        for (UINT32 i = 0; i < STREAMIO_MAX_DISPLAYS; i++) {
            if (!ctx->monitors[i].active) {
                slot = i;
                break;
            }
        }
        if (slot == STREAMIO_MAX_DISPLAYS) {
            status = STATUS_INSUFFICIENT_RESOURCES;
            break;
        }

        /* Create the IddCx monitor */
        IDDCX_MONITOR_INFO monitor_info = {};
        monitor_info.Size = sizeof(monitor_info);
        monitor_info.MonitorType = DISPLAYCONFIG_OUTPUT_TECHNOLOGY_OTHER;
        monitor_info.ConnectorIndex = slot;

        /* Generate EDID */
        BYTE edid[128];
        GenerateEdid(edid, req->width, req->height, req->refresh_hz, slot);

        monitor_info.MonitorDescription.Size = sizeof(monitor_info.MonitorDescription);
        monitor_info.MonitorDescription.Type = IDDCX_MONITOR_DESCRIPTION_TYPE_EDID;
        monitor_info.MonitorDescription.DataSize = 128;
        monitor_info.MonitorDescription.pData = edid;

        IDARG_IN_MONITORCREATE create_in = {};
        create_in.ObjectAttributes = WDF_NO_OBJECT_ATTRIBUTES;
        create_in.pMonitorInfo = &monitor_info;

        IDARG_OUT_MONITORCREATE create_out = {};
        status = IddCxMonitorCreate(ctx->adapter, &create_in, &create_out);

        if (NT_SUCCESS(status)) {
            ctx->monitors[slot].display_id = slot;
            ctx->monitors[slot].width = req->width;
            ctx->monitors[slot].height = req->height;
            ctx->monitors[slot].refresh_hz = req->refresh_hz;
            ctx->monitors[slot].active = TRUE;
            ctx->monitors[slot].monitor_object = create_out.MonitorObject;
            ctx->monitors[slot].stop_thread = FALSE;
            ctx->monitor_count++;

            /* Tell IddCx the monitor arrived (connected) */
            IDARG_OUT_MONITORARRIVAL arrival_out = {};
            IddCxMonitorArrival(create_out.MonitorObject, &arrival_out);

            resp->display_id = slot;
            resp->status = 0;
            WdfRequestSetInformation(request, sizeof(*resp));
        }
        break;
    }

    case IOCTL_DISPLAY_DESTROY: {
        STREAMIO_DISPLAY_DESTROY_REQUEST* req = nullptr;
        status = WdfRequestRetrieveInputBuffer(request, sizeof(*req), (PVOID*)&req, NULL);
        if (!NT_SUCCESS(status)) break;

        if (req->display_id >= STREAMIO_MAX_DISPLAYS || !ctx->monitors[req->display_id].active) {
            status = STATUS_NOT_FOUND;
            break;
        }

        MONITOR_CONTEXT* mon = &ctx->monitors[req->display_id];
        mon->stop_thread = TRUE;

        /* Tell IddCx the monitor departed */
        IddCxMonitorDeparture(mon->monitor_object);

        mon->active = FALSE;
        mon->monitor_object = nullptr;
        ctx->monitor_count--;

        status = STATUS_SUCCESS;
        break;
    }

    case IOCTL_DISPLAY_LIST: {
        STREAMIO_DISPLAY_LIST_RESPONSE* resp = nullptr;
        status = WdfRequestRetrieveOutputBuffer(request, sizeof(*resp), (PVOID*)&resp, NULL);
        if (!NT_SUCCESS(status)) break;

        memset(resp, 0, sizeof(*resp));
        UINT32 count = 0;
        for (UINT32 i = 0; i < STREAMIO_MAX_DISPLAYS; i++) {
            if (ctx->monitors[i].active) {
                resp->displays[count].display_id = i;
                resp->displays[count].width = ctx->monitors[i].width;
                resp->displays[count].height = ctx->monitors[i].height;
                resp->displays[count].refresh_hz = ctx->monitors[i].refresh_hz;
                resp->displays[count].active = 1;
                count++;
            }
        }
        resp->count = count;
        WdfRequestSetInformation(request, sizeof(*resp));
        status = STATUS_SUCCESS;
        break;
    }

    default:
        status = STATUS_INVALID_DEVICE_REQUEST;
        break;
    }

    WdfWaitLockRelease(ctx->lock);
    WdfRequestComplete(request, status);
}

/* ── Device setup ─────────────────────────────────────────────────── */

NTSTATUS EvtDeviceD0Entry(WDFDEVICE device, WDF_POWER_STATE PreviousState)
{
    UNREFERENCED_PARAMETER(PreviousState);

    DEVICE_CONTEXT* ctx = GetDeviceContext(device);

    /* Initialize the IddCx adapter */
    IDD_CX_CLIENT_CONFIG config = {};
    config.Size = sizeof(config);
    config.EvtIddCxAdapterInitFinished = EvtAdapterInitFinished;
    config.EvtIddCxAdapterCommitModes = EvtAdapterCommitModes;
    config.EvtIddCxParseMonitorDescription = EvtParseMonitorDescription;
    config.EvtIddCxMonitorGetDefaultDescriptionModes = EvtMonitorGetDefaultModes;
    config.EvtIddCxMonitorQueryTargetModes = EvtMonitorQueryTargetModes;
    config.EvtIddCxMonitorAssignSwapChain = EvtMonitorAssignSwapChain;
    config.EvtIddCxMonitorUnassignSwapChain = EvtMonitorUnassignSwapChain;

    NTSTATUS status = IddCxDeviceInitConfig(nullptr, &config);
    if (!NT_SUCCESS(status)) return status;

    IDARG_IN_ADAPTER_INIT adapter_init = {};
    adapter_init.WdfDevice = device;
    adapter_init.pCaps = nullptr; /* Use defaults */

    IDARG_OUT_ADAPTER_INIT adapter_out = {};
    status = IddCxAdapterInitAsync(&adapter_init, &adapter_out);
    if (NT_SUCCESS(status)) {
        ctx->adapter = adapter_out.AdapterObject;
    }

    return status;
}

NTSTATUS EvtDeviceAdd(WDFDRIVER driver, PWDFDEVICE_INIT device_init)
{
    UNREFERENCED_PARAMETER(driver);

    /* Configure IddCx device */
    IDD_CX_CLIENT_CONFIG idd_config = {};
    idd_config.Size = sizeof(idd_config);
    idd_config.EvtIddCxAdapterInitFinished = EvtAdapterInitFinished;
    idd_config.EvtIddCxAdapterCommitModes = EvtAdapterCommitModes;
    idd_config.EvtIddCxParseMonitorDescription = EvtParseMonitorDescription;
    idd_config.EvtIddCxMonitorGetDefaultDescriptionModes = EvtMonitorGetDefaultModes;
    idd_config.EvtIddCxMonitorQueryTargetModes = EvtMonitorQueryTargetModes;
    idd_config.EvtIddCxMonitorAssignSwapChain = EvtMonitorAssignSwapChain;
    idd_config.EvtIddCxMonitorUnassignSwapChain = EvtMonitorUnassignSwapChain;

    NTSTATUS status = IddCxDeviceInitConfig(device_init, &idd_config);
    if (!NT_SUCCESS(status)) return status;

    /* Device attributes with context */
    WDF_OBJECT_ATTRIBUTES attrs;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attrs, DEVICE_CONTEXT);

    WDFDEVICE device;
    status = WdfDeviceCreate(&device_init, &attrs, &device);
    if (!NT_SUCCESS(status)) return status;

    DEVICE_CONTEXT* ctx = GetDeviceContext(device);
    memset(ctx, 0, sizeof(*ctx));
    ctx->device = device;

    /* Create a lock for monitor array access */
    WDF_OBJECT_ATTRIBUTES lock_attrs;
    WDF_OBJECT_ATTRIBUTES_INIT(&lock_attrs);
    lock_attrs.ParentObject = device;
    WdfWaitLockCreate(&lock_attrs, &ctx->lock);

    /* Create device interface for user-mode control */
    status = WdfDeviceCreateDeviceInterface(device, &GUID_DEVINTERFACE_STREAMIO_DISPLAY, NULL);
    if (!NT_SUCCESS(status)) return status;

    /* Create default I/O queue for IOCTLs */
    WDF_IO_QUEUE_CONFIG queue_config;
    WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&queue_config, WdfIoQueueDispatchSequential);
    queue_config.EvtIoDeviceControl = EvtIoDeviceControl;

    WDFQUEUE queue;
    status = WdfIoQueueCreate(device, &queue_config, WDF_NO_OBJECT_ATTRIBUTES, &queue);
    if (!NT_SUCCESS(status)) return status;

    /* Initialize adapter */
    IDDCX_ADAPTER_CAPS caps = {};
    caps.Size = sizeof(caps);
    caps.MaxMonitorsSupported = STREAMIO_MAX_DISPLAYS;

    IDARG_IN_ADAPTER_INIT adapter_init = {};
    adapter_init.WdfDevice = device;
    adapter_init.pCaps = &caps;

    IDARG_OUT_ADAPTER_INIT adapter_out = {};
    status = IddCxAdapterInitAsync(&adapter_init, &adapter_out);
    if (NT_SUCCESS(status)) {
        ctx->adapter = adapter_out.AdapterObject;
    }

    return status;
}

/* ── Driver entry ─────────────────────────────────────────────────── */

extern "C" NTSTATUS DriverEntry(PDRIVER_OBJECT driver_object, PUNICODE_STRING registry_path)
{
    WDF_DRIVER_CONFIG config;
    WDF_DRIVER_CONFIG_INIT(&config, EvtDeviceAdd);

    return WdfDriverCreate(driver_object, registry_path, WDF_NO_OBJECT_ATTRIBUTES, &config, WDF_NO_HANDLE);
}
