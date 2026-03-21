/*
 * Streamio Virtual Display Driver (IddCx 1.2, UMDF2)
 *
 * Creates virtual monitors on demand via IOCTL. Each monitor appears as a
 * real display in Windows — GStreamer's d3d11screencapturesrc can capture it
 * independently by monitor-index.
 *
 * Based on Microsoft IddSampleDriver and Virtual-Display-Driver project.
 *
 * Key architecture:
 *   - UMDF2 driver (user-mode, no BSOD risk)
 *   - IddCx framework handles display adapter + monitor lifecycle
 *   - User-mode control via DeviceIoControl (IOCTL_DISPLAY_CREATE/DESTROY/LIST)
 *   - Swapchain: IddCx provides frames via event handle, we acquire+release
 *     to keep the pipeline flowing (GStreamer captures via DXGI independently)
 */

#include <windows.h>
#include <bugcodes.h>
#include <wudfwdm.h>
#include <wdf.h>
#include <IddCx.h>
#include <d3d11.h>
#include <dxgi.h>
#include <d3dkmthk.h>
#include <stdio.h>
#include <stdarg.h>
#include <objbase.h>

#include "streamio-display.h"

/* ── Debug logging (writes to file since UMDF has no DbgPrint) ──── */

static void DriverLog(const char* fmt, ...)
{
    char buf[512];
    va_list args;
    va_start(args, fmt);
    int len = _vsnprintf_s(buf, sizeof(buf), _TRUNCATE, fmt, args);
    va_end(args);
    if (len <= 0) return;

    HANDLE h = CreateFileA("C:\\build\\display-driver.log",
        FILE_APPEND_DATA, FILE_SHARE_READ | FILE_SHARE_WRITE,
        nullptr, OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (h != INVALID_HANDLE_VALUE) {
        DWORD written;
        WriteFile(h, buf, (DWORD)len, &written, nullptr);
        WriteFile(h, "\r\n", 2, &written, nullptr);
        CloseHandle(h);
    }
}

/* Instantiate our GUID (DEFINE_GUID in the header only declares extern) */
extern "C" const GUID GUID_DEVINTERFACE_STREAMIO_DISPLAY =
    { 0xB7E3D5A2, 0x4F1C, 0x8E6D,
      { 0xA9, 0xC0, 0x2B, 0x5D, 0x7F, 0x0E, 0x3A, 0x1C } };

/* ── Per-monitor context (stored as WDF object context on IDDCX_MONITOR) ── */

typedef struct _MONITOR_CONTEXT {
    UINT32 display_id;
    UINT32 width;
    UINT32 height;
    UINT32 refresh_hz;
    IDDCX_SWAPCHAIN swapchain;
    HANDLE swapchain_event;
    HANDLE swapchain_thread;
    volatile BOOLEAN stop_thread;
} MONITOR_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(MONITOR_CONTEXT, GetMonitorContext)

/* ── Per-adapter (device) context ─────────────────────────────────── */

typedef struct _DEVICE_CONTEXT {
    IDDCX_ADAPTER adapter;
    WDFDEVICE device;
    WDFTIMER restore_timer;
    WDFTIMER stability_timer;  /* Clears restore flag after stable operation */
    /* Track active monitors by slot */
    IDDCX_MONITOR monitors[STREAMIO_MAX_DISPLAYS];
    UINT32 widths[STREAMIO_MAX_DISPLAYS];
    UINT32 heights[STREAMIO_MAX_DISPLAYS];
    UINT32 refresh_rates[STREAMIO_MAX_DISPLAYS];
    BOOLEAN active[STREAMIO_MAX_DISPLAYS];
    UINT32 monitor_count;
} DEVICE_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(DEVICE_CONTEXT, GetDeviceContext)

/* ── Global device reference (for EvtAdapterInitFinished) ─────────── */

static WDFDEVICE g_wdf_device = nullptr;

/* ── File-based persistence for monitor config ────────────────────── */
/* UMDF drivers can't write HKLM, so use a config file instead.
 * Format: one line per monitor "slot,width,height,hz\r\n" */

#define STREAMIO_CONFIG_FILE "C:\\ProgramData\\Streamio\\displays.cfg"
#define STREAMIO_RESTORE_FLAG "C:\\ProgramData\\Streamio\\restoring.flag"

/* Crash loop protection:
 * Before restoring, we create a flag file. After 10 seconds of stable
 * operation, we delete it. If the flag exists at restore time, it means
 * the previous restore caused a crash — skip restore to break the loop. */

static BOOLEAN IsRestoreSafe()
{
    DWORD attr = GetFileAttributesA(STREAMIO_RESTORE_FLAG);
    if (attr != INVALID_FILE_ATTRIBUTES) {
        DriverLog("Restore flag exists — previous restore crashed, skipping");
        /* Delete the flag so next manual enable/disable can try again */
        DeleteFileA(STREAMIO_RESTORE_FLAG);
        return FALSE;
    }
    return TRUE;
}

static void SetRestoreFlag()
{
    CreateDirectoryA("C:\\ProgramData\\Streamio", nullptr);
    HANDLE fh = CreateFileA(STREAMIO_RESTORE_FLAG,
        GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (fh != INVALID_HANDLE_VALUE) CloseHandle(fh);
}

static void ClearRestoreFlag()
{
    DeleteFileA(STREAMIO_RESTORE_FLAG);
}

static void SaveAllMonitorsToFile(DEVICE_CONTEXT* ctx)
{
    CreateDirectoryA("C:\\ProgramData\\Streamio", nullptr);

    HANDLE fh = CreateFileA(STREAMIO_CONFIG_FILE,
        GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (fh == INVALID_HANDLE_VALUE) {
        DriverLog("SaveAllMonitorsToFile: CreateFile failed: %lu", GetLastError());
        return;
    }

    DWORD written;
    for (UINT32 i = 0; i < STREAMIO_MAX_DISPLAYS; i++) {
        if (!ctx->active[i]) continue;
        char line[64];
        int len = sprintf_s(line, sizeof(line), "%u,%u,%u,%u\r\n",
            i, ctx->widths[i], ctx->heights[i], ctx->refresh_rates[i]);
        if (len > 0) WriteFile(fh, line, (DWORD)len, &written, nullptr);
    }
    CloseHandle(fh);
    DriverLog("SaveAllMonitorsToFile: saved %u monitors", ctx->monitor_count);
}

static UINT32 LoadMonitorsFromFile(
    UINT32 out_slots[], UINT32 out_widths[], UINT32 out_heights[], UINT32 out_hz[], UINT32 max_count)
{
    HANDLE fh = CreateFileA(STREAMIO_CONFIG_FILE,
        GENERIC_READ, FILE_SHARE_READ, nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (fh == INVALID_HANDLE_VALUE) return 0;

    char buf[1024];
    DWORD bytes_read;
    ReadFile(fh, buf, sizeof(buf) - 1, &bytes_read, nullptr);
    CloseHandle(fh);
    buf[bytes_read] = '\0';

    UINT32 count = 0;
    char* line = buf;
    while (*line && count < max_count) {
        UINT32 slot, w, h_val, hz;
        if (sscanf_s(line, "%u,%u,%u,%u", &slot, &w, &h_val, &hz) == 4) {
            if (slot < STREAMIO_MAX_DISPLAYS) {
                out_slots[count] = slot;
                out_widths[count] = w;
                out_heights[count] = h_val;
                out_hz[count] = hz;
                count++;
            }
        }
        while (*line && *line != '\n') line++;
        if (*line == '\n') line++;
    }
    return count;
}

/* ── D3D device singleton (for swapchain processing) ──────────────── */

static ID3D11Device* g_d3d_device = nullptr;
static IDXGIAdapter* g_d3d_adapter = nullptr;

/* Try to create a D3D11 device on a specific DXGI adapter.
 * IddCx requires the device to be on the render adapter it assigned,
 * so we try each hardware adapter until IddCxSwapChainSetDevice succeeds.
 * We store the adapter index that worked for reuse. */
static int g_working_adapter_index = -1;

static ID3D11Device* CreateD3DDeviceOnAdapter(int adapter_index)
{
    IDXGIFactory1* factory = nullptr;
    HRESULT hr = CreateDXGIFactory1(__uuidof(IDXGIFactory1), (void**)&factory);
    if (FAILED(hr) || !factory) {
        DriverLog("CreateDXGIFactory1 failed: 0x%08X", hr);
        return nullptr;
    }

    IDXGIAdapter1* adapter = nullptr;
    hr = factory->EnumAdapters1((UINT)adapter_index, &adapter);
    factory->Release();
    if (FAILED(hr) || !adapter) {
        return nullptr;
    }

    DXGI_ADAPTER_DESC1 desc;
    adapter->GetDesc1(&desc);
    DriverLog("Trying D3D11 on adapter %d: %.64ls (VendorId=0x%04X)",
              adapter_index, desc.Description, desc.VendorId);

    D3D_FEATURE_LEVEL levels[] = { D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1,
                                   D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_9_1 };
    D3D_FEATURE_LEVEL level_out;
    ID3D11Device* dev = nullptr;
    hr = D3D11CreateDevice(
        adapter, D3D_DRIVER_TYPE_UNKNOWN, nullptr,
        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        levels, ARRAYSIZE(levels), D3D11_SDK_VERSION,
        &dev, &level_out, nullptr
    );
    if (FAILED(hr)) {
        DriverLog("D3D11CreateDevice on adapter %d failed: 0x%08X", adapter_index, hr);
        adapter->Release();
        return nullptr;
    }

    DriverLog("D3D11 device created on adapter %d (level=0x%X)", adapter_index, (UINT)level_out);
    if (g_d3d_adapter) g_d3d_adapter->Release();
    g_d3d_adapter = adapter;
    return dev;
}

static ID3D11Device* GetOrCreateD3DDevice()
{
    if (g_d3d_device) return g_d3d_device;

    /* If we know which adapter works, use it directly */
    if (g_working_adapter_index >= 0) {
        g_d3d_device = CreateD3DDeviceOnAdapter(g_working_adapter_index);
        return g_d3d_device;
    }

    /* Try each hardware adapter */
    for (int i = 0; i < 8; i++) {
        g_d3d_device = CreateD3DDeviceOnAdapter(i);
        if (g_d3d_device) return g_d3d_device;
    }

    /* Last resort: WARP software adapter */
    DriverLog("All hardware adapters failed, trying WARP");
    D3D_FEATURE_LEVEL levels[] = { D3D_FEATURE_LEVEL_11_0 };
    D3D_FEATURE_LEVEL level_out;
    HRESULT hr = D3D11CreateDevice(
        nullptr, D3D_DRIVER_TYPE_WARP, nullptr,
        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        levels, 1, D3D11_SDK_VERSION,
        &g_d3d_device, &level_out, nullptr
    );
    if (FAILED(hr)) {
        DriverLog("WARP also failed: 0x%08X", hr);
        g_d3d_device = nullptr;
    }
    return g_d3d_device;
}

/* ── EDID generation ──────────────────────────────────────────────── */

static void GenerateEdid(BYTE edid[128], UINT32 width, UINT32 height, UINT32 refresh_hz, UINT32 display_id)
{
    memset(edid, 0, 128);

    /* Header */
    edid[0] = 0x00; edid[1] = 0xFF; edid[2] = 0xFF; edid[3] = 0xFF;
    edid[4] = 0xFF; edid[5] = 0xFF; edid[6] = 0xFF; edid[7] = 0x00;

    /* Manufacturer: "STR" (Streamio) */
    edid[8] = (BYTE)(((0x13 - 1) << 2) | ((0x14 - 1) >> 3));
    edid[9] = (BYTE)(((0x14 - 1) << 5) | (0x12 - 1));

    edid[10] = (BYTE)(0x01 + display_id); edid[11] = 0x00;
    edid[12] = 0x01; edid[13] = 0x00; edid[14] = 0x00; edid[15] = 0x00;

    edid[16] = 1;  /* week */
    edid[17] = 34; /* year 2024 */
    edid[18] = 1;  /* EDID 1.3 */
    edid[19] = 3;
    edid[20] = 0x80; /* digital */

    edid[21] = (BYTE)(width * 53 / 1920);
    edid[22] = (BYTE)(height * 30 / 1080);
    edid[23] = 120; /* gamma 2.2 */
    edid[24] = 0x0A;

    /* sRGB chromaticity */
    edid[25] = 0xEE; edid[26] = 0x91; edid[27] = 0xA3; edid[28] = 0x54;
    edid[29] = 0x4C; edid[30] = 0x99; edid[31] = 0x26; edid[32] = 0x0F;
    edid[33] = 0x50; edid[34] = 0x54;

    /* Standard timings: unused */
    for (int i = 38; i < 54; i += 2) { edid[i] = 0x01; edid[i + 1] = 0x01; }

    /* Detailed Timing Descriptor #1 */
    UINT32 htotal = width + width / 5;
    UINT32 vtotal = height + height / 20;
    UINT32 pixel_clock = (UINT32)((UINT64)htotal * vtotal * refresh_hz / 10000);
    UINT32 hblank = htotal - width;
    UINT32 vblank = vtotal - height;

    edid[54] = (BYTE)(pixel_clock & 0xFF);
    edid[55] = (BYTE)((pixel_clock >> 8) & 0xFF);
    edid[56] = (BYTE)(width & 0xFF);
    edid[57] = (BYTE)(hblank & 0xFF);
    edid[58] = (BYTE)(((width >> 8) << 4) | ((hblank >> 8) & 0x0F));
    edid[59] = (BYTE)(height & 0xFF);
    edid[60] = (BYTE)(vblank & 0xFF);
    edid[61] = (BYTE)(((height >> 8) << 4) | ((vblank >> 8) & 0x0F));
    edid[62] = 48; edid[63] = 32; edid[64] = 0x35; edid[65] = 0x00;

    UINT32 h_mm = width * 530 / 1920;
    UINT32 v_mm = height * 300 / 1080;
    edid[66] = (BYTE)(h_mm & 0xFF);
    edid[67] = (BYTE)(v_mm & 0xFF);
    edid[68] = (BYTE)(((h_mm >> 8) << 4) | ((v_mm >> 8) & 0x0F));
    edid[69] = 0; edid[70] = 0;
    edid[71] = 0x18; /* non-interlaced, digital */

    /* Monitor name */
    edid[72] = 0; edid[73] = 0; edid[74] = 0; edid[75] = 0xFC; edid[76] = 0;
    char name[14];
    int name_len = sprintf_s(name, sizeof(name), "Streamio %u", display_id);
    for (int i = 0; i < 13; i++) edid[77 + i] = (i < name_len) ? (BYTE)name[i] : 0x0A;

    /* Range limits */
    edid[90] = 0; edid[91] = 0; edid[92] = 0; edid[93] = 0xFD; edid[94] = 0;
    edid[95] = (BYTE)(refresh_hz - 1);
    edid[96] = (BYTE)(refresh_hz + 1);
    edid[97] = 30; edid[98] = 150; edid[99] = 25; edid[100] = 0x00;

    /* Serial */
    edid[108] = 0; edid[109] = 0; edid[110] = 0; edid[111] = 0xFF; edid[112] = 0;
    char serial[14];
    int serial_len = sprintf_s(serial, sizeof(serial), "STRVD%07u", display_id);
    for (int i = 0; i < 13; i++) edid[113 + i] = (i < serial_len) ? (BYTE)serial[i] : 0x0A;

    edid[126] = 0; /* no extensions */
    BYTE sum = 0;
    for (int i = 0; i < 127; i++) sum += edid[i];
    edid[127] = (BYTE)(256 - sum);
}

/* ── Swapchain processing thread ──────────────────────────────────── */

static DWORD WINAPI SwapchainThread(LPVOID param)
{
    MONITOR_CONTEXT* ctx = (MONITOR_CONTEXT*)param;

    DriverLog("SwapchainThread: started for monitor %u", ctx->display_id);

    __try {
        while (!ctx->stop_thread) {
            if (!ctx->swapchain_event || !ctx->swapchain) {
                Sleep(100);
                continue;
            }

            DWORD wait = WaitForSingleObject(ctx->swapchain_event, 100);
            if (wait != WAIT_OBJECT_0) continue;
            if (ctx->stop_thread) break;

            IDDCX_SWAPCHAIN sc = ctx->swapchain;
            if (!sc) break;

            IDARG_OUT_RELEASEANDACQUIREBUFFER buf_out = {};

            NTSTATUS status = IddCxSwapChainReleaseAndAcquireBuffer(sc, &buf_out);
            if (NT_SUCCESS(status)) {
                IddCxSwapChainFinishedProcessingFrame(sc);
            } else if (status == STATUS_GRAPHICS_INDIRECT_DISPLAY_ABANDON_SWAPCHAIN) {
                DriverLog("SwapchainThread: swapchain abandoned for monitor %u", ctx->display_id);
                break;
            }
        }
    }
    __except (EXCEPTION_EXECUTE_HANDLER) {
        DriverLog("SwapchainThread: EXCEPTION 0x%08X in monitor %u", GetExceptionCode(), ctx->display_id);
    }

    DriverLog("SwapchainThread: exiting for monitor %u", ctx->display_id);
    return 0;
}

/* ── IddCx Callbacks ─────────────────────────────────────────────── */

/* Forward declaration — creates a monitor on an adapter */
static NTSTATUS CreateMonitorOnAdapter(DEVICE_CONTEXT* ctx, UINT32 slot,
    UINT32 width, UINT32 height, UINT32 refresh_hz);

/* Stability timer — if we get here (10s after restore), the restore didn't crash us */
void EvtStabilityTimerFunc(WDFTIMER timer)
{
    UNREFERENCED_PARAMETER(timer);
    ClearRestoreFlag();
    DriverLog("StabilityTimer: restore flag cleared — driver is stable");
}

/* Timer callback — restores monitors after D0 cycles settle */
void EvtRestoreTimerFunc(WDFTIMER timer)
{
    WDFDEVICE device = (WDFDEVICE)WdfTimerGetParentObject(timer);
    DEVICE_CONTEXT* ctx = GetDeviceContext(device);

    if (ctx->adapter == nullptr) {
        DriverLog("RestoreTimer: no adapter, skipping");
        return;
    }
    if (ctx->monitor_count > 0) {
        DriverLog("RestoreTimer: %u monitors already active, skipping", ctx->monitor_count);
        return;
    }

    /* Crash loop protection: if previous restore crashed, skip */
    if (!IsRestoreSafe()) {
        DriverLog("RestoreTimer: skipping restore (crash loop protection)");
        return;
    }

    UINT32 slots[STREAMIO_MAX_DISPLAYS], widths[STREAMIO_MAX_DISPLAYS];
    UINT32 heights[STREAMIO_MAX_DISPLAYS], rates[STREAMIO_MAX_DISPLAYS];
    UINT32 count = LoadMonitorsFromFile(slots, widths, heights, rates, STREAMIO_MAX_DISPLAYS);

    if (count == 0) {
        DriverLog("RestoreTimer: no monitors to restore");
        return;
    }

    /* Set flag BEFORE restoring — if we crash, next restart will see it */
    SetRestoreFlag();

    for (UINT32 i = 0; i < count; i++) {
        DriverLog("RestoreTimer: restoring slot=%u %ux%u@%u", slots[i], widths[i], heights[i], rates[i]);
        NTSTATUS st = CreateMonitorOnAdapter(ctx, slots[i], widths[i], heights[i], rates[i]);
        if (!NT_SUCCESS(st)) {
            DriverLog("RestoreTimer: failed slot=%u: 0x%08X", slots[i], st);
        }
    }
    SaveAllMonitorsToFile(ctx);

    /* Schedule stability check: if we survive 10s, clear the flag */
    if (ctx->stability_timer) {
        WdfTimerStart(ctx->stability_timer, WDF_REL_TIMEOUT_IN_SEC(10));
    }
}

NTSTATUS EvtAdapterInitFinished(
    IDDCX_ADAPTER adapter,
    const IDARG_IN_ADAPTER_INIT_FINISHED* args)
{
    UNREFERENCED_PARAMETER(adapter);
    DriverLog("AdapterInitFinished: status=0x%08X", args->AdapterInitStatus);
    if (!NT_SUCCESS(args->AdapterInitStatus)) return args->AdapterInitStatus;

    /* Defer monitor restore by 3 seconds to let D0 cycles settle.
     * Without this delay, creating monitors triggers display reconfiguration
     * which causes another D0 cycle, cascading into Code 43. */
    if (g_wdf_device) {
        DEVICE_CONTEXT* ctx = GetDeviceContext(g_wdf_device);
        if (ctx->restore_timer && ctx->monitor_count == 0) {
            DriverLog("AdapterInitFinished: scheduling deferred restore (3s)");
            WdfTimerStart(ctx->restore_timer, WDF_REL_TIMEOUT_IN_SEC(3));
        }
    }

    return STATUS_SUCCESS;
}

NTSTATUS EvtAdapterCommitModes(
    IDDCX_ADAPTER adapter,
    const IDARG_IN_COMMITMODES* args)
{
    UNREFERENCED_PARAMETER(adapter);
    UNREFERENCED_PARAMETER(args);
    return STATUS_SUCCESS;
}

NTSTATUS EvtParseMonitorDescription(
    const IDARG_IN_PARSEMONITORDESCRIPTION* in_args,
    IDARG_OUT_PARSEMONITORDESCRIPTION* out_args)
{
    /* Parse our EDID to extract the single preferred mode */
    if (in_args->MonitorDescription.DataSize < 128 || !in_args->MonitorDescription.pData) {
        return STATUS_INVALID_PARAMETER;
    }

    BYTE* edid = (BYTE*)in_args->MonitorDescription.pData;

    /* Extract resolution from DTD #1 (bytes 54-71) */
    UINT32 width = edid[56] | ((edid[58] >> 4) << 8);
    UINT32 height = edid[59] | ((edid[61] >> 4) << 8);
    UINT32 pixel_clock = edid[54] | (edid[55] << 8); /* in 10kHz */

    UINT32 hblank = edid[57] | ((edid[58] & 0x0F) << 8);
    UINT32 vblank = edid[60] | ((edid[61] & 0x0F) << 8);
    UINT32 htotal = width + hblank;
    UINT32 vtotal = height + vblank;

    UINT32 refresh_hz = 60;
    if (htotal > 0 && vtotal > 0 && pixel_clock > 0) {
        refresh_hz = (UINT32)((UINT64)pixel_clock * 10000 / ((UINT64)htotal * vtotal));
    }

    /* Build monitor mode — match Microsoft IddSampleDriver pattern exactly:
     * totalSize == activeSize (no blanking), scanLineOrdering = PROGRESSIVE,
     * pixelRate = vsync * w * h, hSyncFreq = vsync * height */
    IDDCX_MONITOR_MODE mode = {};
    mode.Size = sizeof(mode);
    mode.Origin = IDDCX_MONITOR_MODE_ORIGIN_MONITORDESCRIPTOR;
    mode.MonitorVideoSignalInfo.totalSize.cx = width;
    mode.MonitorVideoSignalInfo.totalSize.cy = height;
    mode.MonitorVideoSignalInfo.activeSize.cx = width;
    mode.MonitorVideoSignalInfo.activeSize.cy = height;
    mode.MonitorVideoSignalInfo.AdditionalSignalInfo.vSyncFreqDivider = 0;
    mode.MonitorVideoSignalInfo.AdditionalSignalInfo.videoStandard = 255;
    mode.MonitorVideoSignalInfo.vSyncFreq.Numerator = refresh_hz;
    mode.MonitorVideoSignalInfo.vSyncFreq.Denominator = 1;
    mode.MonitorVideoSignalInfo.hSyncFreq.Numerator = refresh_hz * height;
    mode.MonitorVideoSignalInfo.hSyncFreq.Denominator = 1;
    mode.MonitorVideoSignalInfo.scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;
    mode.MonitorVideoSignalInfo.pixelRate = (UINT64)refresh_hz * (UINT64)width * (UINT64)height;

    if (in_args->MonitorModeBufferInputCount == 0) {
        out_args->MonitorModeBufferOutputCount = 1;
    } else {
        in_args->pMonitorModes[0] = mode;
        out_args->MonitorModeBufferOutputCount = 1;
    }
    out_args->PreferredMonitorModeIdx = 0;

    return STATUS_SUCCESS;
}

NTSTATUS EvtMonitorGetDefaultModes(
    IDDCX_MONITOR monitor,
    const IDARG_IN_GETDEFAULTDESCRIPTIONMODES* in_args,
    IDARG_OUT_GETDEFAULTDESCRIPTIONMODES* out_args)
{
    /* Not used — we provide EDID and parse it in EvtParseMonitorDescription */
    UNREFERENCED_PARAMETER(monitor);
    UNREFERENCED_PARAMETER(in_args);
    out_args->DefaultMonitorModeBufferOutputCount = 0;
    out_args->PreferredMonitorModeIdx = NO_PREFERRED_MODE;
    return STATUS_SUCCESS;
}

NTSTATUS EvtMonitorQueryTargetModes(
    IDDCX_MONITOR monitor,
    const IDARG_IN_QUERYTARGETMODES* in_args,
    IDARG_OUT_QUERYTARGETMODES* out_args)
{
    MONITOR_CONTEXT* ctx_mon = GetMonitorContext(monitor);

    UINT32 w = ctx_mon->width;
    UINT32 h = ctx_mon->height;
    UINT32 hz = ctx_mon->refresh_hz;

    /* Match Microsoft IddSampleDriver target mode pattern:
     * totalSize == activeSize, vSyncFreqDivider=1, videoStandard=255 */
    IDDCX_TARGET_MODE mode = {};
    mode.Size = sizeof(mode);
    DISPLAYCONFIG_VIDEO_SIGNAL_INFO& vsi = mode.TargetVideoSignalInfo.targetVideoSignalInfo;
    vsi.totalSize.cx = w;
    vsi.totalSize.cy = h;
    vsi.activeSize.cx = w;
    vsi.activeSize.cy = h;
    vsi.AdditionalSignalInfo.vSyncFreqDivider = 1;
    vsi.AdditionalSignalInfo.videoStandard = 255;
    vsi.vSyncFreq.Numerator = hz;
    vsi.vSyncFreq.Denominator = 1;
    vsi.hSyncFreq.Numerator = hz * h;
    vsi.hSyncFreq.Denominator = 1;
    vsi.scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;
    vsi.pixelRate = (UINT64)hz * (UINT64)w * (UINT64)h;

    if (in_args->TargetModeBufferInputCount == 0) {
        out_args->TargetModeBufferOutputCount = 1;
    } else {
        in_args->pTargetModes[0] = mode;
        out_args->TargetModeBufferOutputCount = 1;
    }

    return STATUS_SUCCESS;
}

NTSTATUS EvtMonitorAssignSwapChain(
    IDDCX_MONITOR monitor,
    const IDARG_IN_SETSWAPCHAIN* args)
{
    MONITOR_CONTEXT* ctx = GetMonitorContext(monitor);

    DriverLog("AssignSwapChain: monitor=%u swapchain=%p", ctx->display_id, args->hSwapChain);

    /* Try to set the D3D device on the swapchain.
     * IddCx requires the device on the correct render adapter.
     * If the current device fails, try each adapter until one works. */
    BOOLEAN device_set = FALSE;

    for (int attempt = 0; attempt < 2 && !device_set; attempt++) {
        ID3D11Device* dev = GetOrCreateD3DDevice();
        if (!dev) {
            DriverLog("AssignSwapChain: D3D device creation failed");
            break;
        }

        IDXGIDevice* dxgi_dev = nullptr;
        HRESULT hr = dev->QueryInterface(__uuidof(IDXGIDevice), (void**)&dxgi_dev);
        if (FAILED(hr) || !dxgi_dev) {
            DriverLog("AssignSwapChain: QueryInterface(IDXGIDevice) failed: 0x%08X", hr);
            break;
        }

        IDARG_IN_SWAPCHAINSETDEVICE set_dev = {};
        set_dev.pDevice = dxgi_dev;
        NTSTATUS st = IddCxSwapChainSetDevice(args->hSwapChain, &set_dev);
        dxgi_dev->Release();

        if (NT_SUCCESS(st)) {
            DriverLog("AssignSwapChain: device set successfully (adapter %d)", g_working_adapter_index);
            device_set = TRUE;
        } else {
            DriverLog("AssignSwapChain: IddCxSwapChainSetDevice failed: 0x%08X (adapter %d), trying next",
                      st, g_working_adapter_index);
            /* Release current device and try next adapter */
            g_d3d_device->Release();
            g_d3d_device = nullptr;
            g_working_adapter_index++;
        }
    }

    /* If first round didn't work, try all remaining adapters */
    if (!device_set) {
        int start = (g_working_adapter_index >= 0) ? g_working_adapter_index : 0;
        for (int i = start; i < 8 && !device_set; i++) {
            if (g_d3d_device) { g_d3d_device->Release(); g_d3d_device = nullptr; }
            g_d3d_device = CreateD3DDeviceOnAdapter(i);
            if (!g_d3d_device) continue;

            IDXGIDevice* dxgi_dev = nullptr;
            HRESULT hr = g_d3d_device->QueryInterface(__uuidof(IDXGIDevice), (void**)&dxgi_dev);
            if (FAILED(hr) || !dxgi_dev) continue;

            IDARG_IN_SWAPCHAINSETDEVICE set_dev = {};
            set_dev.pDevice = dxgi_dev;
            NTSTATUS st = IddCxSwapChainSetDevice(args->hSwapChain, &set_dev);
            dxgi_dev->Release();

            if (NT_SUCCESS(st)) {
                g_working_adapter_index = i;
                DriverLog("AssignSwapChain: SUCCESS on adapter %d", i);
                device_set = TRUE;
            } else {
                DriverLog("AssignSwapChain: adapter %d failed: 0x%08X", i, st);
            }
        }
    }

    if (!device_set) {
        DriverLog("AssignSwapChain: all adapters failed, swapchain will not process frames");
        return STATUS_SUCCESS;
    }

    ctx->swapchain = args->hSwapChain;
    ctx->swapchain_event = args->hNextSurfaceAvailable;
    ctx->stop_thread = FALSE;

    /* Start frame processing thread only after D3D device is properly set */
    ctx->swapchain_thread = CreateThread(nullptr, 0, SwapchainThread, ctx, 0, nullptr);
    DriverLog("AssignSwapChain: thread started for monitor %u", ctx->display_id);

    return STATUS_SUCCESS;
}

NTSTATUS EvtMonitorUnassignSwapChain(IDDCX_MONITOR monitor)
{
    MONITOR_CONTEXT* ctx = GetMonitorContext(monitor);

    DriverLog("UnassignSwapChain: monitor=%u", ctx->display_id);

    ctx->stop_thread = TRUE;
    if (ctx->swapchain_thread) {
        WaitForSingleObject(ctx->swapchain_thread, 5000);
        CloseHandle(ctx->swapchain_thread);
        ctx->swapchain_thread = nullptr;
    }
    ctx->swapchain = nullptr;
    ctx->swapchain_event = nullptr;

    return STATUS_SUCCESS;
}

/* ── Monitor creation helper (shared by IOCTL + adapter-init restore) ── */

static NTSTATUS CreateMonitorOnAdapter(DEVICE_CONTEXT* ctx, UINT32 slot,
    UINT32 width, UINT32 height, UINT32 refresh_hz)
{
    if (slot >= STREAMIO_MAX_DISPLAYS || ctx->adapter == nullptr)
        return STATUS_INVALID_PARAMETER;

    if (ctx->active[slot])
        return STATUS_OBJECTID_EXISTS;

    BYTE edid[128];
    GenerateEdid(edid, width, height, refresh_hz, slot);

    IDDCX_MONITOR_INFO mon_info = {};
    mon_info.Size = sizeof(mon_info);
    mon_info.MonitorType = DISPLAYCONFIG_OUTPUT_TECHNOLOGY_OTHER;
    mon_info.ConnectorIndex = slot;
    mon_info.MonitorDescription.Size = sizeof(mon_info.MonitorDescription);
    mon_info.MonitorDescription.Type = IDDCX_MONITOR_DESCRIPTION_TYPE_EDID;
    mon_info.MonitorDescription.DataSize = 128;
    mon_info.MonitorDescription.pData = edid;
    CoCreateGuid(&mon_info.MonitorContainerId);

    WDF_OBJECT_ATTRIBUTES mon_attrs;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&mon_attrs, MONITOR_CONTEXT);

    IDARG_IN_MONITORCREATE create_in = {};
    create_in.ObjectAttributes = &mon_attrs;
    create_in.pMonitorInfo = &mon_info;

    IDARG_OUT_MONITORCREATE create_out = {};
    NTSTATUS status = IddCxMonitorCreate(ctx->adapter, &create_in, &create_out);
    if (!NT_SUCCESS(status)) return status;

    MONITOR_CONTEXT* mon_ctx = GetMonitorContext(create_out.MonitorObject);
    mon_ctx->display_id = slot;
    mon_ctx->width = width;
    mon_ctx->height = height;
    mon_ctx->refresh_hz = refresh_hz;
    mon_ctx->swapchain = nullptr;
    mon_ctx->swapchain_event = nullptr;
    mon_ctx->swapchain_thread = nullptr;
    mon_ctx->stop_thread = FALSE;

    IDARG_OUT_MONITORARRIVAL arrival_out = {};
    status = IddCxMonitorArrival(create_out.MonitorObject, &arrival_out);
    if (!NT_SUCCESS(status)) return status;

    ctx->monitors[slot] = create_out.MonitorObject;
    ctx->widths[slot] = width;
    ctx->heights[slot] = height;
    ctx->refresh_rates[slot] = refresh_hz;
    ctx->active[slot] = TRUE;
    ctx->monitor_count++;

    DriverLog("Monitor %u created: %ux%u@%uHz", slot, width, height, refresh_hz);
    return STATUS_SUCCESS;
}

/* ── IOCTL handler (via IDD_CX_CLIENT_CONFIG.EvtIddCxDeviceIoControl) ── */

void EvtDeviceIoControl(
    WDFDEVICE device,
    WDFREQUEST request,
    size_t output_length,
    size_t input_length,
    ULONG ioctl_code)
{
    UNREFERENCED_PARAMETER(output_length);
    UNREFERENCED_PARAMETER(input_length);

    DEVICE_CONTEXT* ctx = GetDeviceContext(device);
    NTSTATUS status = STATUS_INVALID_PARAMETER;

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

        /* Find free slot */
        UINT32 slot = STREAMIO_MAX_DISPLAYS;
        for (UINT32 i = 0; i < STREAMIO_MAX_DISPLAYS; i++) {
            if (!ctx->active[i]) { slot = i; break; }
        }
        if (slot == STREAMIO_MAX_DISPLAYS) {
            status = STATUS_INSUFFICIENT_RESOURCES;
            break;
        }

        status = CreateMonitorOnAdapter(ctx, slot, req->width, req->height, req->refresh_hz);
        if (!NT_SUCCESS(status)) break;

        /* Persist to registry so it survives power cycles */
        SaveAllMonitorsToFile(ctx);

        resp->display_id = slot;
        resp->status = 0;
        WdfRequestSetInformation(request, sizeof(*resp));
        break;
    }

    case IOCTL_DISPLAY_DESTROY: {
        STREAMIO_DISPLAY_DESTROY_REQUEST* req = nullptr;
        status = WdfRequestRetrieveInputBuffer(request, sizeof(*req), (PVOID*)&req, NULL);
        if (!NT_SUCCESS(status)) break;

        if (req->display_id >= STREAMIO_MAX_DISPLAYS || !ctx->active[req->display_id]) {
            status = STATUS_NOT_FOUND;
            break;
        }

        IddCxMonitorDeparture(ctx->monitors[req->display_id]);
        ctx->active[req->display_id] = FALSE;
        ctx->monitors[req->display_id] = nullptr;
        ctx->monitor_count--;
        SaveAllMonitorsToFile(ctx);
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
            if (ctx->active[i]) {
                resp->displays[count].display_id = i;
                resp->displays[count].width = ctx->widths[i];
                resp->displays[count].height = ctx->heights[i];
                resp->displays[count].refresh_hz = ctx->refresh_rates[i];
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

    WdfRequestComplete(request, status);
}

/* ── D0 Entry: adapter init happens here (not in EvtDeviceAdd) ────── */

NTSTATUS EvtDeviceD0Entry(WDFDEVICE device, WDF_POWER_DEVICE_STATE prev_state)
{
    UNREFERENCED_PARAMETER(prev_state);

    DEVICE_CONTEXT* ctx = GetDeviceContext(device);

    /* Release stale D3D device from previous cycle — adapter may have changed */
    if (g_d3d_device) {
        g_d3d_device->Release();
        g_d3d_device = nullptr;
    }
    if (g_d3d_adapter) {
        g_d3d_adapter->Release();
        g_d3d_adapter = nullptr;
    }

    /* Only init adapter once */
    if (ctx->adapter != nullptr) return STATUS_SUCCESS;

    /* Adapter capabilities — must fill EndPointDiagnostics */
    IDDCX_ADAPTER_CAPS caps = {};
    caps.Size = sizeof(caps);
    caps.MaxMonitorsSupported = STREAMIO_MAX_DISPLAYS;

    caps.EndPointDiagnostics.Size = sizeof(caps.EndPointDiagnostics);
    caps.EndPointDiagnostics.GammaSupport = IDDCX_FEATURE_IMPLEMENTATION_NONE;
    caps.EndPointDiagnostics.TransmissionType = IDDCX_TRANSMISSION_TYPE_WIRED_OTHER;

    caps.EndPointDiagnostics.pEndPointFriendlyName = L"Streamio Virtual Display";
    caps.EndPointDiagnostics.pEndPointManufacturerName = L"Streamio";
    caps.EndPointDiagnostics.pEndPointModelName = L"Streamio VDD";

    IDDCX_ENDPOINT_VERSION fw_ver = {};
    fw_ver.Size = sizeof(fw_ver);
    fw_ver.MajorVer = 1;
    caps.EndPointDiagnostics.pFirmwareVersion = &fw_ver;

    IDDCX_ENDPOINT_VERSION hw_ver = {};
    hw_ver.Size = sizeof(hw_ver);
    hw_ver.MajorVer = 1;
    caps.EndPointDiagnostics.pHardwareVersion = &hw_ver;

    IDARG_IN_ADAPTER_INIT adapter_init = {};
    adapter_init.WdfDevice = device;
    adapter_init.pCaps = &caps;

    IDARG_OUT_ADAPTER_INIT adapter_out = {};
    NTSTATUS status = IddCxAdapterInitAsync(&adapter_init, &adapter_out);
    DriverLog("D0Entry: IddCxAdapterInitAsync status=0x%08X adapter=%p",
              status, NT_SUCCESS(status) ? adapter_out.AdapterObject : nullptr);
    if (NT_SUCCESS(status)) {
        ctx->adapter = adapter_out.AdapterObject;
    }

    return status;
}

/* ── Device setup ─────────────────────────────────────────────────── */

NTSTATUS EvtDeviceAdd(WDFDRIVER driver, PWDFDEVICE_INIT device_init)
{
    UNREFERENCED_PARAMETER(driver);

    /* Register PnP power callback — adapter init happens in D0Entry */
    WDF_PNPPOWER_EVENT_CALLBACKS pnp_callbacks;
    WDF_PNPPOWER_EVENT_CALLBACKS_INIT(&pnp_callbacks);
    pnp_callbacks.EvtDeviceD0Entry = EvtDeviceD0Entry;
    WdfDeviceInitSetPnpPowerEventCallbacks(device_init, &pnp_callbacks);

    /* IddCx must configure the device BEFORE WdfDeviceCreate */
    IDD_CX_CLIENT_CONFIG idd_config;
    IDD_CX_CLIENT_CONFIG_INIT(&idd_config);
    idd_config.EvtIddCxDeviceIoControl = EvtDeviceIoControl;
    idd_config.EvtIddCxParseMonitorDescription = EvtParseMonitorDescription;
    idd_config.EvtIddCxAdapterInitFinished = EvtAdapterInitFinished;
    idd_config.EvtIddCxAdapterCommitModes = EvtAdapterCommitModes;
    idd_config.EvtIddCxMonitorGetDefaultDescriptionModes = EvtMonitorGetDefaultModes;
    idd_config.EvtIddCxMonitorQueryTargetModes = EvtMonitorQueryTargetModes;
    idd_config.EvtIddCxMonitorAssignSwapChain = EvtMonitorAssignSwapChain;
    idd_config.EvtIddCxMonitorUnassignSwapChain = EvtMonitorUnassignSwapChain;

    NTSTATUS status = IddCxDeviceInitConfig(device_init, &idd_config);
    if (!NT_SUCCESS(status)) return status;

    /* Create the WDF device with our context */
    WDF_OBJECT_ATTRIBUTES attrs;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attrs, DEVICE_CONTEXT);

    WDFDEVICE device;
    status = WdfDeviceCreate(&device_init, &attrs, &device);
    if (!NT_SUCCESS(status)) return status;

    DEVICE_CONTEXT* ctx = GetDeviceContext(device);
    memset(ctx, 0, sizeof(*ctx));
    ctx->device = device;
    g_wdf_device = device;

    /* Create deferred restore timer (fires 3s after adapter init) */
    WDF_TIMER_CONFIG timer_config;
    WDF_TIMER_CONFIG_INIT(&timer_config, EvtRestoreTimerFunc);
    timer_config.AutomaticSerialization = FALSE;
    WDF_OBJECT_ATTRIBUTES timer_attrs;
    WDF_OBJECT_ATTRIBUTES_INIT(&timer_attrs);
    timer_attrs.ParentObject = device;
    timer_attrs.ExecutionLevel = WdfExecutionLevelPassive;
    status = WdfTimerCreate(&timer_config, &timer_attrs, &ctx->restore_timer);
    if (!NT_SUCCESS(status)) {
        DriverLog("WdfTimerCreate(restore) failed: 0x%08X", status);
        ctx->restore_timer = nullptr;
    }

    /* Create stability timer (fires 10s after successful restore to clear crash flag) */
    WDF_TIMER_CONFIG stab_config;
    WDF_TIMER_CONFIG_INIT(&stab_config, EvtStabilityTimerFunc);
    stab_config.AutomaticSerialization = FALSE;
    WDF_OBJECT_ATTRIBUTES stab_attrs;
    WDF_OBJECT_ATTRIBUTES_INIT(&stab_attrs);
    stab_attrs.ParentObject = device;
    stab_attrs.ExecutionLevel = WdfExecutionLevelPassive;
    status = WdfTimerCreate(&stab_config, &stab_attrs, &ctx->stability_timer);
    if (!NT_SUCCESS(status)) {
        DriverLog("WdfTimerCreate(stability) failed: 0x%08X", status);
        ctx->stability_timer = nullptr;
    }

    /* Finalize IddCx device initialization */
    status = IddCxDeviceInitialize(device);
    if (!NT_SUCCESS(status)) return status;

    /* Create device interface for user-mode control (display-ctl) */
    status = WdfDeviceCreateDeviceInterface(device, &GUID_DEVINTERFACE_STREAMIO_DISPLAY, NULL);
    if (!NT_SUCCESS(status)) return status;

    return STATUS_SUCCESS;
}

/* ── Driver entry ─────────────────────────────────────────────────── */

extern "C" NTSTATUS DriverEntry(PDRIVER_OBJECT driver_object, PUNICODE_STRING registry_path)
{
    WDF_DRIVER_CONFIG config;
    WDF_DRIVER_CONFIG_INIT(&config, EvtDeviceAdd);

    return WdfDriverCreate(driver_object, registry_path, WDF_NO_OBJECT_ATTRIBUTES, &config, WDF_NO_HANDLE);
}
