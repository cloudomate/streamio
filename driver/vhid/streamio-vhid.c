/*
 * streamio-vhid.c - Streamio Virtual HID Driver (KMDF + VHF)
 *
 * Creates a virtual keyboard + mouse device using Microsoft's
 * Virtual HID Framework. User-mode sends HID reports via IOCTLs,
 * and the driver submits them to the OS input stack via VHF.
 *
 * This operates at the HID level, so input works on the lock screen,
 * UAC prompts, and secure desktop — bypassing UIPI entirely.
 */

#include <ntddk.h>
#include <wdf.h>
#include <vhf.h>

#include "streamio-vhid.h"
#include "hid-descriptor.h"

/* Driver tag for memory allocations */
#define POOL_TAG 'dihV'

/* Device context stored per WDF device */
typedef struct _DEVICE_CONTEXT {
    VHFHANDLE VhfHandle;
} DEVICE_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(DEVICE_CONTEXT, GetDeviceContext)

/* Forward declarations */
EVT_WDF_DRIVER_DEVICE_ADD   EvtDeviceAdd;
EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL EvtIoDeviceControl;

/* ================================================================
 * DriverEntry - Driver initialization
 * ================================================================ */
NTSTATUS
DriverEntry(
    _In_ PDRIVER_OBJECT  DriverObject,
    _In_ PUNICODE_STRING RegistryPath
)
{
    WDF_DRIVER_CONFIG config;
    NTSTATUS status;

    WDF_DRIVER_CONFIG_INIT(&config, EvtDeviceAdd);

    status = WdfDriverCreate(
        DriverObject,
        RegistryPath,
        WDF_NO_OBJECT_ATTRIBUTES,
        &config,
        WDF_NO_HANDLE
    );

    return status;
}

/* ================================================================
 * EvtDeviceAdd - Create device, set up VHF, create I/O queue
 * ================================================================ */
NTSTATUS
EvtDeviceAdd(
    _In_ WDFDRIVER Driver,
    _Inout_ PWDFDEVICE_INIT DeviceInit
)
{
    NTSTATUS status;
    WDFDEVICE device;
    DEVICE_CONTEXT* devCtx;
    WDF_OBJECT_ATTRIBUTES deviceAttributes;
    VHF_CONFIG vhfConfig;
    WDF_IO_QUEUE_CONFIG queueConfig;
    WDFQUEUE queue;

    UNREFERENCED_PARAMETER(Driver);

    /* Create the WDF device with context */
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&deviceAttributes, DEVICE_CONTEXT);

    status = WdfDeviceCreate(&DeviceInit, &deviceAttributes, &device);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    devCtx = GetDeviceContext(device);
    devCtx->VhfHandle = NULL;

    /* Create a device interface so user-mode can find us */
    status = WdfDeviceCreateDeviceInterface(
        device,
        &GUID_DEVINTERFACE_STREAMIO_VHID,
        NULL
    );
    if (!NT_SUCCESS(status)) {
        return status;
    }

    /* ---- Initialize VHF ---- */
    VHF_CONFIG_INIT(
        &vhfConfig,
        WdfDeviceWdmGetDeviceObject(device),
        STREAMIO_HID_REPORT_DESCRIPTOR_SIZE,
        (PUCHAR)StreamioHidReportDescriptor
    );

    /* Vendor/Product IDs (arbitrary, identifies our device) */
    vhfConfig.VendorID  = 0x1234;
    vhfConfig.ProductID = 0x5678;
    vhfConfig.VersionNumber = 0x0001;

    status = VhfCreate(&vhfConfig, &devCtx->VhfHandle);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    status = VhfStart(devCtx->VhfHandle);
    if (!NT_SUCCESS(status)) {
        VhfDelete(devCtx->VhfHandle, FALSE);
        devCtx->VhfHandle = NULL;
        return status;
    }

    /* ---- Create default I/O queue for IOCTLs ---- */
    WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&queueConfig, WdfIoQueueDispatchParallel);
    queueConfig.EvtIoDeviceControl = EvtIoDeviceControl;

    status = WdfIoQueueCreate(device, &queueConfig, WDF_NO_OBJECT_ATTRIBUTES, &queue);
    if (!NT_SUCCESS(status)) {
        VhfDelete(devCtx->VhfHandle, FALSE);
        devCtx->VhfHandle = NULL;
        return status;
    }

    return STATUS_SUCCESS;
}

/* ================================================================
 * EvtIoDeviceControl - Handle IOCTLs from user-mode
 * ================================================================ */
VOID
EvtIoDeviceControl(
    _In_ WDFQUEUE Queue,
    _In_ WDFREQUEST Request,
    _In_ size_t OutputBufferLength,
    _In_ size_t InputBufferLength,
    _In_ ULONG IoControlCode
)
{
    NTSTATUS status = STATUS_SUCCESS;
    DEVICE_CONTEXT* devCtx;
    WDFDEVICE device;
    PVOID inputBuffer;
    size_t bufferSize;
    HID_XFER_PACKET hidReport;

    UNREFERENCED_PARAMETER(OutputBufferLength);

    device = WdfIoQueueGetDevice(Queue);
    devCtx = GetDeviceContext(device);

    if (devCtx->VhfHandle == NULL) {
        WdfRequestComplete(Request, STATUS_DEVICE_NOT_READY);
        return;
    }

    switch (IoControlCode) {

    case IOCTL_VHID_SUBMIT_KEYBOARD:
    {
        if (InputBufferLength < sizeof(VHID_KEYBOARD_REPORT)) {
            WdfRequestComplete(Request, STATUS_BUFFER_TOO_SMALL);
            return;
        }

        status = WdfRequestRetrieveInputBuffer(
            Request, sizeof(VHID_KEYBOARD_REPORT), &inputBuffer, &bufferSize);
        if (!NT_SUCCESS(status)) {
            WdfRequestComplete(Request, status);
            return;
        }

        /* Submit keyboard report to VHF */
        hidReport.reportBuffer = (PUCHAR)inputBuffer;
        hidReport.reportBufferLen = sizeof(VHID_KEYBOARD_REPORT);
        hidReport.reportId = 1;

        status = VhfReadReportSubmit(devCtx->VhfHandle, &hidReport);
        WdfRequestComplete(Request, status);
        return;
    }

    case IOCTL_VHID_SUBMIT_MOUSE:
    {
        if (InputBufferLength < sizeof(VHID_MOUSE_REPORT)) {
            WdfRequestComplete(Request, STATUS_BUFFER_TOO_SMALL);
            return;
        }

        status = WdfRequestRetrieveInputBuffer(
            Request, sizeof(VHID_MOUSE_REPORT), &inputBuffer, &bufferSize);
        if (!NT_SUCCESS(status)) {
            WdfRequestComplete(Request, status);
            return;
        }

        /* Submit mouse report to VHF */
        hidReport.reportBuffer = (PUCHAR)inputBuffer;
        hidReport.reportBufferLen = sizeof(VHID_MOUSE_REPORT);
        hidReport.reportId = 2;

        status = VhfReadReportSubmit(devCtx->VhfHandle, &hidReport);
        WdfRequestComplete(Request, status);
        return;
    }

    default:
        WdfRequestComplete(Request, STATUS_INVALID_DEVICE_REQUEST);
        return;
    }
}
