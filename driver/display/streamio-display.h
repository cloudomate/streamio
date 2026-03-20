/*
 * Streamio Virtual Display Driver — shared definitions
 *
 * IddCx UMDF2 driver that creates virtual monitors on demand.
 * Controlled via DeviceIoControl from user-mode (display-ctl).
 */

#pragma once

#include <windows.h>

/* Device interface GUID — used by SetupDiGetClassDevs to find the device */
/* {B7E3D5A2-4F1C-8E6D-A9C0-2B5D7F0E3A1C} */
DEFINE_GUID(GUID_DEVINTERFACE_STREAMIO_DISPLAY,
    0xB7E3D5A2, 0x4F1C, 0x8E6D,
    0xA9, 0xC0, 0x2B, 0x5D, 0x7F, 0x0E, 0x3A, 0x1C);

/* IOCTL codes */
#define FILE_DEVICE_STREAMIO_DISPLAY  0x8001

#define IOCTL_DISPLAY_CREATE  CTL_CODE(FILE_DEVICE_STREAMIO_DISPLAY, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_DISPLAY_DESTROY CTL_CODE(FILE_DEVICE_STREAMIO_DISPLAY, 0x801, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_DISPLAY_LIST    CTL_CODE(FILE_DEVICE_STREAMIO_DISPLAY, 0x802, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_DISPLAY_UPDATE  CTL_CODE(FILE_DEVICE_STREAMIO_DISPLAY, 0x803, METHOD_BUFFERED, FILE_ANY_ACCESS)

/* Maximum virtual displays supported */
#define STREAMIO_MAX_DISPLAYS  16

/* Request to create a virtual display */
typedef struct _STREAMIO_DISPLAY_CREATE_REQUEST {
    UINT32 width;       /* e.g. 1920 */
    UINT32 height;      /* e.g. 1080 */
    UINT32 refresh_hz;  /* e.g. 60 */
} STREAMIO_DISPLAY_CREATE_REQUEST;

/* Response after creating a display */
typedef struct _STREAMIO_DISPLAY_CREATE_RESPONSE {
    UINT32 display_id;  /* 0-based index */
    UINT32 status;      /* 0 = success */
} STREAMIO_DISPLAY_CREATE_RESPONSE;

/* Request to destroy a virtual display */
typedef struct _STREAMIO_DISPLAY_DESTROY_REQUEST {
    UINT32 display_id;
} STREAMIO_DISPLAY_DESTROY_REQUEST;

/* Request to update display resolution */
typedef struct _STREAMIO_DISPLAY_UPDATE_REQUEST {
    UINT32 display_id;
    UINT32 width;
    UINT32 height;
    UINT32 refresh_hz;
} STREAMIO_DISPLAY_UPDATE_REQUEST;

/* Info about one virtual display */
typedef struct _STREAMIO_DISPLAY_INFO {
    UINT32 display_id;
    UINT32 width;
    UINT32 height;
    UINT32 refresh_hz;
    UINT32 active;      /* 1 if connected */
} STREAMIO_DISPLAY_INFO;

/* Response for LIST ioctl */
typedef struct _STREAMIO_DISPLAY_LIST_RESPONSE {
    UINT32 count;
    STREAMIO_DISPLAY_INFO displays[STREAMIO_MAX_DISPLAYS];
} STREAMIO_DISPLAY_LIST_RESPONSE;
