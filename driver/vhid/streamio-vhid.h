/*
 * streamio-vhid.h - Shared definitions for the Streamio Virtual HID driver
 *
 * IOCTL codes and report structures used by both the KMDF driver
 * and the user-mode service (streamio-service.exe).
 */

#pragma once

#include <initguid.h>

/* Device interface GUID - user-mode opens this to send IOCTLs */
/* {A8B3F5E1-7D2C-4E9A-B6F0-1A3C5D8E2F4B} */
DEFINE_GUID(GUID_DEVINTERFACE_STREAMIO_VHID,
    0xa8b3f5e1, 0x7d2c, 0x4e9a,
    0xb6, 0xf0, 0x1a, 0x3c, 0x5d, 0x8e, 0x2f, 0x4b);

/*
 * IOCTL codes
 *
 * METHOD_BUFFERED: kernel copies input buffer, safe for small payloads.
 * FILE_ANY_ACCESS: no special privileges required beyond device handle.
 */
#define FILE_DEVICE_VHID  0x8000

#define IOCTL_VHID_SUBMIT_KEYBOARD \
    CTL_CODE(FILE_DEVICE_VHID, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS)

#define IOCTL_VHID_SUBMIT_MOUSE \
    CTL_CODE(FILE_DEVICE_VHID, 0x801, METHOD_BUFFERED, FILE_ANY_ACCESS)

/*
 * Keyboard HID report (matches HID report descriptor, Report ID 1)
 *
 * Standard 8-byte boot keyboard report:
 *   Byte 0: Modifier bitmap (bit0=LCtrl, bit1=LShift, bit2=LAlt, bit3=LGUI,
 *                             bit4=RCtrl, bit5=RShift, bit6=RAlt, bit7=RGUI)
 *   Byte 1: Reserved (0x00)
 *   Bytes 2-7: Up to 6 simultaneous USB HID usage codes (Usage Page 0x07)
 */
#pragma pack(push, 1)
typedef struct _VHID_KEYBOARD_REPORT {
    unsigned char ReportId;     /* Must be 1 */
    unsigned char Modifiers;    /* Modifier bitmap */
    unsigned char Reserved;     /* Always 0 */
    unsigned char Keys[6];      /* USB HID keycodes (0 = no key) */
} VHID_KEYBOARD_REPORT;
#pragma pack(pop)

/*
 * Mouse HID report (matches HID report descriptor, Report ID 2)
 *
 * Absolute mouse with 3 buttons + vertical wheel:
 *   Byte 0: Buttons (bit0=left, bit1=right, bit2=middle)
 *   Bytes 1-2: X absolute (0-32767)
 *   Bytes 3-4: Y absolute (0-32767)
 *   Byte 5: Wheel (-127 to +127)
 */
#pragma pack(push, 1)
typedef struct _VHID_MOUSE_REPORT {
    unsigned char ReportId;     /* Must be 2 */
    unsigned char Buttons;      /* Button bitmap */
    short X;                    /* Absolute X: 0-32767 */
    short Y;                    /* Absolute Y: 0-32767 */
    signed char Wheel;          /* Vertical scroll */
} VHID_MOUSE_REPORT;
#pragma pack(pop)
