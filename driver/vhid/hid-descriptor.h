/*
 * hid-descriptor.h - HID Report Descriptors for Streamio Virtual HID
 *
 * Two top-level collections in one descriptor:
 *   Report ID 1: Keyboard (standard boot keyboard, Usage Page 0x07)
 *   Report ID 2: Mouse (absolute pointer, 3 buttons, wheel)
 *
 * Reference: USB HID Usage Tables 1.4, USB HID 1.11 spec
 */

#pragma once

static const unsigned char StreamioHidReportDescriptor[] = {

    /* ================================================================
     * Collection 1: Keyboard (Report ID 1)
     * Standard boot keyboard: 8 modifier bits + 6 keycode array
     * ================================================================ */
    0x05, 0x01,         /* USAGE_PAGE (Generic Desktop) */
    0x09, 0x06,         /* USAGE (Keyboard) */
    0xA1, 0x01,         /* COLLECTION (Application) */
    0x85, 0x01,         /*   REPORT_ID (1) */

    /* Modifier byte: 8 bits for Ctrl/Shift/Alt/GUI (L+R) */
    0x05, 0x07,         /*   USAGE_PAGE (Keyboard/Keypad) */
    0x19, 0xE0,         /*   USAGE_MINIMUM (Left Control) */
    0x29, 0xE7,         /*   USAGE_MAXIMUM (Right GUI) */
    0x15, 0x00,         /*   LOGICAL_MINIMUM (0) */
    0x25, 0x01,         /*   LOGICAL_MAXIMUM (1) */
    0x75, 0x01,         /*   REPORT_SIZE (1) */
    0x95, 0x08,         /*   REPORT_COUNT (8) */
    0x81, 0x02,         /*   INPUT (Data,Var,Abs) -- modifier bits */

    /* Reserved byte */
    0x75, 0x08,         /*   REPORT_SIZE (8) */
    0x95, 0x01,         /*   REPORT_COUNT (1) */
    0x81, 0x01,         /*   INPUT (Const) -- reserved */

    /* LED output report (optional, keeps HID class happy) */
    0x05, 0x08,         /*   USAGE_PAGE (LEDs) */
    0x19, 0x01,         /*   USAGE_MINIMUM (Num Lock) */
    0x29, 0x05,         /*   USAGE_MAXIMUM (Kana) */
    0x75, 0x01,         /*   REPORT_SIZE (1) */
    0x95, 0x05,         /*   REPORT_COUNT (5) */
    0x91, 0x02,         /*   OUTPUT (Data,Var,Abs) -- LED bits */
    0x75, 0x03,         /*   REPORT_SIZE (3) */
    0x95, 0x01,         /*   REPORT_COUNT (1) */
    0x91, 0x01,         /*   OUTPUT (Const) -- LED padding */

    /* Keycode array: 6 simultaneous keys */
    0x05, 0x07,         /*   USAGE_PAGE (Keyboard/Keypad) */
    0x19, 0x00,         /*   USAGE_MINIMUM (Reserved/No Event) */
    0x29, 0xFF,         /*   USAGE_MAXIMUM (0xFF) */
    0x15, 0x00,         /*   LOGICAL_MINIMUM (0) */
    0x26, 0xFF, 0x00,   /*   LOGICAL_MAXIMUM (255) */
    0x75, 0x08,         /*   REPORT_SIZE (8) */
    0x95, 0x06,         /*   REPORT_COUNT (6) */
    0x81, 0x00,         /*   INPUT (Data,Ary,Abs) -- keycode array */

    0xC0,               /* END_COLLECTION */

    /* ================================================================
     * Collection 2: Mouse / Absolute Pointer (Report ID 2)
     * Absolute X/Y (0-32767), 3 buttons, vertical wheel
     * ================================================================ */
    0x05, 0x01,         /* USAGE_PAGE (Generic Desktop) */
    0x09, 0x02,         /* USAGE (Mouse) */
    0xA1, 0x01,         /* COLLECTION (Application) */
    0x85, 0x02,         /*   REPORT_ID (2) */
    0x09, 0x01,         /*   USAGE (Pointer) */
    0xA1, 0x00,         /*   COLLECTION (Physical) */

    /* 3 buttons */
    0x05, 0x09,         /*     USAGE_PAGE (Buttons) */
    0x19, 0x01,         /*     USAGE_MINIMUM (Button 1 - Left) */
    0x29, 0x03,         /*     USAGE_MAXIMUM (Button 3 - Middle) */
    0x15, 0x00,         /*     LOGICAL_MINIMUM (0) */
    0x25, 0x01,         /*     LOGICAL_MAXIMUM (1) */
    0x75, 0x01,         /*     REPORT_SIZE (1) */
    0x95, 0x03,         /*     REPORT_COUNT (3) */
    0x81, 0x02,         /*     INPUT (Data,Var,Abs) -- button bits */

    /* 5 bits padding to complete the button byte */
    0x75, 0x05,         /*     REPORT_SIZE (5) */
    0x95, 0x01,         /*     REPORT_COUNT (1) */
    0x81, 0x01,         /*     INPUT (Const) -- padding */

    /* Absolute X: 0 to 32767 (16-bit) */
    0x05, 0x01,         /*     USAGE_PAGE (Generic Desktop) */
    0x09, 0x30,         /*     USAGE (X) */
    0x15, 0x00,         /*     LOGICAL_MINIMUM (0) */
    0x26, 0xFF, 0x7F,   /*     LOGICAL_MAXIMUM (32767) */
    0x75, 0x10,         /*     REPORT_SIZE (16) */
    0x95, 0x01,         /*     REPORT_COUNT (1) */
    0x81, 0x02,         /*     INPUT (Data,Var,Abs) */

    /* Absolute Y: 0 to 32767 (16-bit) */
    0x09, 0x31,         /*     USAGE (Y) */
    0x15, 0x00,         /*     LOGICAL_MINIMUM (0) */
    0x26, 0xFF, 0x7F,   /*     LOGICAL_MAXIMUM (32767) */
    0x75, 0x10,         /*     REPORT_SIZE (16) */
    0x95, 0x01,         /*     REPORT_COUNT (1) */
    0x81, 0x02,         /*     INPUT (Data,Var,Abs) */

    /* Vertical wheel: -127 to +127 */
    0x09, 0x38,         /*     USAGE (Wheel) */
    0x15, 0x81,         /*     LOGICAL_MINIMUM (-127) */
    0x25, 0x7F,         /*     LOGICAL_MAXIMUM (127) */
    0x75, 0x08,         /*     REPORT_SIZE (8) */
    0x95, 0x01,         /*     REPORT_COUNT (1) */
    0x81, 0x06,         /*     INPUT (Data,Var,Rel) -- wheel is relative */

    0xC0,               /*   END_COLLECTION (Physical) */
    0xC0,               /* END_COLLECTION (Application) */
};

#define STREAMIO_HID_REPORT_DESCRIPTOR_SIZE sizeof(StreamioHidReportDescriptor)
