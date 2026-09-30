/*
 * Crossglide virtual precision touchpad: a UMDF 2 HID minidriver on a root-enumerated device.
 *
 * Windows sees a Precision Touchpad (plus the mouse and configuration collections the PTP spec
 * asks for) and does all gesture recognition itself. The driver has no touch logic: the
 * crossglide agent writes finished input reports to a vendor collection on the same device, and
 * the driver hands each one to HIDCLASS as input. See README.md next to this file.
 *
 * Built from the command line against the WDK NuGet package by build.ps1; no Visual Studio
 * project.
 */

#define WIN32_NO_STATUS
#include <windows.h>
#undef WIN32_NO_STATUS
#include <ntstatus.h>
#include <wdf.h>
#include <hidport.h>

#define REPORTID_TOUCHPAD 0x01
#define REPORTID_MOUSE 0x02
#define REPORTID_CAPS 0x03
#define REPORTID_PTPHQA 0x04
#define REPORTID_INPUT_MODE 0x05
#define REPORTID_FUNCTION_SWITCH 0x06
#define REPORTID_FEED 0x07

#define CONTACTS 5

/* Report sizes, with the report ID byte. */
#define TOUCHPAD_REPORT_LEN (1 + CONTACTS * 5 + 2 + 1 + 1)
#define MOUSE_REPORT_LEN (1 + 1 + 2 + 2 + 1)
/* Feed report body: the target report (ID first) padded with zeros. */
#define FEED_LEN 32

/*
 * Surface of a MacBook Air 13" (M4) trackpad, 121.9 x 74.1 mm. Positions are in 0.05 mm
 * (logical), the physical extent in 0.1 mm; that is 508 dpi, over the 300 the spec asks for.
 */
#define X_LOGICAL_MAX 2438
#define Y_LOGICAL_MAX 1482
#define X_PHYSICAL_MAX 1219
#define Y_PHYSICAL_MAX 741

#define LE16(v) (UCHAR)((v) & 0xff), (UCHAR)(((v) >> 8) & 0xff)

#define FINGER                                                                              \
    0x05, 0x0d,                   /*   Usage Page (Digitizers) */                           \
    0x09, 0x22,                   /*   Usage (Finger) */                                    \
    0xa1, 0x02,                   /*   Collection (Logical) */                              \
    0x35, 0x00, 0x45, 0x00,       /*     Physical Min/Max (0): reset from the last finger */ \
    0x55, 0x00, 0x65, 0x00,       /*     Unit Exponent (0), Unit (None) */                  \
    0x15, 0x00, 0x25, 0x01,       /*     Logical Min (0), Max (1) */                        \
    0x75, 0x01, 0x95, 0x02,       /*     Report Size (1), Count (2) */                      \
    0x09, 0x47, 0x09, 0x42,       /*     Usage (Confidence), Usage (Tip Switch) */          \
    0x81, 0x02,                   /*     Input (Data, Var, Abs) */                          \
    0x25, 0x07, 0x75, 0x03,       /*     Logical Max (7), Report Size (3) */                \
    0x95, 0x01, 0x09, 0x51,       /*     Report Count (1), Usage (Contact Identifier) */    \
    0x81, 0x02,                   /*     Input (Data, Var, Abs) */                          \
    0x81, 0x03,                   /*     Input (Const): 3 bits of padding */                \
    0x05, 0x01,                   /*     Usage Page (Generic Desktop) */                    \
    0x75, 0x10,                   /*     Report Size (16) */                                \
    0x55, 0x0e, 0x65, 0x11,       /*     Unit Exponent (-2), Unit (cm): 0.1 mm */           \
    0x26, LE16(X_LOGICAL_MAX),    /*     Logical Max */                                     \
    0x46, LE16(X_PHYSICAL_MAX),   /*     Physical Max */                                    \
    0x09, 0x30, 0x81, 0x02,       /*     Usage (X), Input (Data, Var, Abs) */               \
    0x26, LE16(Y_LOGICAL_MAX),    /*     Logical Max */                                     \
    0x46, LE16(Y_PHYSICAL_MAX),   /*     Physical Max */                                    \
    0x09, 0x31, 0x81, 0x02,       /*     Usage (Y), Input (Data, Var, Abs) */               \
    0xc0                          /*   End Collection */

static const UCHAR ReportDescriptor[] = {
    /* Precision touchpad */
    0x05, 0x0d,                   /* Usage Page (Digitizers) */
    0x09, 0x05,                   /* Usage (Touch Pad) */
    0xa1, 0x01,                   /* Collection (Application) */
    0x85, REPORTID_TOUCHPAD,
    FINGER, FINGER, FINGER, FINGER, FINGER,
    0x55, 0x0c,                   /*   Unit Exponent (-4) */
    0x66, 0x01, 0x10,             /*   Unit (Seconds): scan time in 100 us */
    0x47, 0xff, 0xff, 0x00, 0x00, /*   Physical Max (65535) */
    0x27, 0xff, 0xff, 0x00, 0x00, /*   Logical Max (65535) */
    0x75, 0x10, 0x95, 0x01,       /*   Report Size (16), Count (1) */
    0x05, 0x0d, 0x09, 0x56,       /*   Usage Page (Digitizers), Usage (Scan Time) */
    0x81, 0x02,                   /*   Input (Data, Var, Abs) */
    0x35, 0x00, 0x45, 0x00,       /*   Physical Min/Max (0) */
    0x55, 0x00, 0x65, 0x00,       /*   Unit Exponent (0), Unit (None) */
    0x09, 0x54, 0x25, 0x7f,       /*   Usage (Contact Count), Logical Max (127) */
    0x75, 0x08, 0x95, 0x01,       /*   Report Size (8), Count (1) */
    0x81, 0x02,                   /*   Input (Data, Var, Abs) */
    0x05, 0x09, 0x09, 0x01,       /*   Usage Page (Button), Usage (Button 1) */
    0x25, 0x01, 0x75, 0x01,       /*   Logical Max (1), Report Size (1) */
    0x95, 0x01, 0x81, 0x02,       /*   Report Count (1), Input (Data, Var, Abs) */
    0x95, 0x07, 0x81, 0x03,       /*   Report Count (7), Input (Const) */
    0x05, 0x0d,                   /*   Usage Page (Digitizers) */
    0x85, REPORTID_CAPS,
    0x09, 0x55, 0x09, 0x59,       /*   Usage (Contact Count Maximum), Usage (Pad Type) */
    0x75, 0x04, 0x95, 0x02,       /*   Report Size (4), Count (2) */
    0x25, 0x0f, 0xb1, 0x02,       /*   Logical Max (15), Feature (Data, Var, Abs) */
    0x06, 0x00, 0xff,             /*   Usage Page (Vendor 0xFF00) */
    0x85, REPORTID_PTPHQA,
    0x09, 0xc5,                   /*   Usage (0xC5): certification status */
    0x15, 0x00, 0x26, 0xff, 0x00, /*   Logical Min (0), Max (255) */
    0x75, 0x08, 0x96, 0x00, 0x01, /*   Report Size (8), Count (256) */
    0xb1, 0x02,                   /*   Feature (Data, Var, Abs) */
    0xc0,                         /* End Collection */

    /* Configuration */
    0x05, 0x0d,                   /* Usage Page (Digitizers) */
    0x09, 0x0e,                   /* Usage (Device Configuration) */
    0xa1, 0x01,                   /* Collection (Application) */
    0x85, REPORTID_INPUT_MODE,
    0x09, 0x22, 0xa1, 0x02,       /*   Usage (Finger), Collection (Logical) */
    0x09, 0x52,                   /*     Usage (Input Mode) */
    0x15, 0x00, 0x25, 0x0a,       /*     Logical Min (0), Max (10) */
    0x75, 0x08, 0x95, 0x01,       /*     Report Size (8), Count (1) */
    0xb1, 0x02,                   /*     Feature (Data, Var, Abs) */
    0xc0,                         /*   End Collection */
    0x09, 0x22, 0xa1, 0x00,       /*   Usage (Finger), Collection (Physical) */
    0x85, REPORTID_FUNCTION_SWITCH,
    0x09, 0x57, 0x09, 0x58,       /*     Usage (Surface Switch), Usage (Button Switch) */
    0x75, 0x01, 0x95, 0x02,       /*     Report Size (1), Count (2) */
    0x25, 0x01, 0xb1, 0x02,       /*     Logical Max (1), Feature (Data, Var, Abs) */
    0x95, 0x06, 0xb1, 0x03,       /*     Report Count (6), Feature (Const) */
    0xc0,                         /*   End Collection */
    0xc0,                         /* End Collection */

    /* Mouse, for legacy mode (and later a Mac mouse) */
    0x05, 0x01, 0x09, 0x02,       /* Usage Page (Generic Desktop), Usage (Mouse) */
    0xa1, 0x01,                   /* Collection (Application) */
    0x85, REPORTID_MOUSE,
    0x09, 0x01, 0xa1, 0x00,       /*   Usage (Pointer), Collection (Physical) */
    0x05, 0x09, 0x19, 0x01,       /*     Usage Page (Button), Usage Min (1) */
    0x29, 0x03, 0x15, 0x00,       /*     Usage Max (3), Logical Min (0) */
    0x25, 0x01, 0x75, 0x01,       /*     Logical Max (1), Report Size (1) */
    0x95, 0x03, 0x81, 0x02,       /*     Report Count (3), Input (Data, Var, Abs) */
    0x95, 0x05, 0x81, 0x03,       /*     Report Count (5), Input (Const) */
    0x05, 0x01, 0x09, 0x30,       /*     Usage Page (Generic Desktop), Usage (X) */
    0x09, 0x31,                   /*     Usage (Y) */
    0x16, 0x01, 0x80,             /*     Logical Min (-32767) */
    0x26, 0xff, 0x7f,             /*     Logical Max (32767) */
    0x75, 0x10, 0x95, 0x02,       /*     Report Size (16), Count (2) */
    0x81, 0x06,                   /*     Input (Data, Var, Rel) */
    0x09, 0x38,                   /*     Usage (Wheel) */
    0x15, 0x81, 0x25, 0x7f,       /*     Logical Min (-127), Max (127) */
    0x75, 0x08, 0x95, 0x01,       /*     Report Size (8), Count (1) */
    0x81, 0x06,                   /*     Input (Data, Var, Rel) */
    0xc0,                         /*   End Collection */
    0xc0,                         /* End Collection */

    /* Feed: the agent writes input reports here */
    0x06, 0x42, 0xff,             /* Usage Page (Vendor 0xFF42) */
    0x09, 0x01,                   /* Usage (1) */
    0xa1, 0x01,                   /* Collection (Application) */
    0x85, REPORTID_FEED,
    0x09, 0x02,                   /*   Usage (2) */
    0x15, 0x00, 0x26, 0xff, 0x00, /*   Logical Min (0), Max (255) */
    0x75, 0x08, 0x95, FEED_LEN,   /*   Report Size (8), Count */
    0x91, 0x02,                   /*   Output (Data, Var, Abs) */
    0xc0,                         /* End Collection */
};

static const HID_DESCRIPTOR HidDescriptor = {
    sizeof(HID_DESCRIPTOR),
    HID_HID_DESCRIPTOR_TYPE,
    0x0101,
    0,
    1,
    {{HID_REPORT_DESCRIPTOR_TYPE, sizeof(ReportDescriptor)}},
};

/* pid.codes' open-source vendor ID; the product ID is unallocated, for local use only. */
#define VENDOR_ID 0x1209
#define PRODUCT_ID 0xC6D0
#define VERSION 0x0100

/*
 * Windows' sample certification-status blob, as every open-source precision touchpad driver
 * returns it. Windows asks for it; it doesn't make the device certified.
 */
static const UCHAR CertificationBlob[256] = {
    0xfc, 0x28, 0xfe, 0x84, 0x40, 0xcb, 0x9a, 0x87, 0x0d, 0xbe, 0x57, 0x3c, 0xb6, 0x70, 0x09, 0x88,
    0x07, 0x97, 0x2d, 0x2b, 0xe3, 0x38, 0x34, 0xb6, 0x6c, 0xed, 0xb0, 0xf7, 0xe5, 0x9c, 0xf6, 0xc2,
    0x2e, 0x84, 0x1b, 0xe8, 0xb4, 0x51, 0x78, 0x43, 0x1f, 0x28, 0x4b, 0x7c, 0x2d, 0x53, 0xaf, 0xfc,
    0x47, 0x70, 0x1b, 0x59, 0x6f, 0x74, 0x43, 0xc4, 0xf3, 0x47, 0x18, 0x53, 0x1a, 0xa2, 0xa1, 0x71,
    0xc7, 0x95, 0x0e, 0x31, 0x55, 0x21, 0xd3, 0xb5, 0x1e, 0xe9, 0x0c, 0xba, 0xec, 0xb8, 0x89, 0x19,
    0x3e, 0xb3, 0xaf, 0x75, 0x81, 0x9d, 0x53, 0xb9, 0x41, 0x57, 0xf4, 0x6d, 0x39, 0x25, 0x29, 0x7c,
    0x87, 0xd9, 0xb4, 0x98, 0x45, 0x7d, 0xa7, 0x26, 0x9c, 0x65, 0x3b, 0x85, 0x68, 0x89, 0xd7, 0x3b,
    0xbd, 0xff, 0x14, 0x67, 0xf2, 0x2b, 0xf0, 0x2a, 0x41, 0x54, 0xf0, 0xfd, 0x2c, 0x66, 0x7c, 0xf8,
    0xc0, 0x8f, 0x33, 0x13, 0x03, 0xf1, 0xd3, 0xc1, 0x0b, 0x89, 0xd9, 0x1b, 0x62, 0xcd, 0x51, 0xb7,
    0x80, 0xb8, 0xaf, 0x3a, 0x10, 0xc1, 0x8a, 0x5b, 0xe8, 0x8a, 0x56, 0xf0, 0x8c, 0xaa, 0xfa, 0x35,
    0xe9, 0x42, 0xc4, 0xd8, 0x55, 0xc3, 0x38, 0xcc, 0x2b, 0x53, 0x5c, 0x69, 0x52, 0xd5, 0xc8, 0x73,
    0x02, 0x38, 0x7c, 0x73, 0xb6, 0x41, 0xe7, 0xff, 0x05, 0xd8, 0x2b, 0x79, 0x9a, 0xe2, 0x34, 0x60,
    0x8f, 0xa3, 0x32, 0x1f, 0x09, 0x78, 0x62, 0xbc, 0x80, 0xe3, 0x0f, 0xbd, 0x65, 0x20, 0x08, 0x13,
    0xc1, 0xe2, 0xee, 0x53, 0x2d, 0x86, 0x7e, 0xa7, 0x5a, 0xc5, 0xd3, 0x7d, 0x98, 0xbe, 0x31, 0x48,
    0x1f, 0xfb, 0xda, 0xaf, 0xa2, 0xa8, 0x6a, 0x89, 0xd6, 0xbf, 0xf2, 0xd3, 0x32, 0x2a, 0x9a, 0xe4,
    0xcf, 0x17, 0xb7, 0xb8, 0xf4, 0xe1, 0x33, 0x08, 0x24, 0x8b, 0xc4, 0x43, 0xa5, 0xe5, 0x24, 0xc2,
};

static const WCHAR Manufacturer[] = L"Crossglide";
static const WCHAR Product[] = L"Crossglide Virtual Touchpad";
static const WCHAR SerialNumber[] = L"0001";

/* Reports written while HIDCLASS had no read waiting, delivered as reads arrive. */
#define BACKLOG 16

typedef struct {
    UCHAR Data[FEED_LEN];
    ULONG Length;
} REPORT;

typedef struct {
    /* HIDCLASS's pending IOCTL_HID_READ_REPORTs. */
    WDFQUEUE ReadQueue;
    REPORT Backlog[BACKLOG];
    ULONG BacklogStart;
    ULONG BacklogCount;
    UCHAR InputMode;
    UCHAR FunctionSwitch;
} DEVICE_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(DEVICE_CONTEXT, GetContext)

DRIVER_INITIALIZE DriverEntry;
EVT_WDF_DRIVER_DEVICE_ADD EvtDeviceAdd;
EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL EvtIoDeviceControl;

/* Copies `Length` bytes of `Source` to the request's output buffer. */
static NTSTATUS CopyToRequest(WDFREQUEST Request, const void *Source, size_t Length)
{
    WDFMEMORY memory;
    size_t capacity;
    NTSTATUS status = WdfRequestRetrieveOutputMemory(Request, &memory);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    WdfMemoryGetBuffer(memory, &capacity);
    if (capacity < Length) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    status = WdfMemoryCopyFromBuffer(memory, 0, (PVOID)Source, Length);
    if (NT_SUCCESS(status)) {
        WdfRequestSetInformation(Request, Length);
    }
    return status;
}

/* Length of an input report the feed may carry, by its report ID; 0 for any other ID. */
static ULONG InputReportLength(UCHAR ReportId)
{
    switch (ReportId) {
    case REPORTID_TOUCHPAD:
        return TOUCHPAD_REPORT_LEN;
    case REPORTID_MOUSE:
        return MOUSE_REPORT_LEN;
    default:
        return 0;
    }
}

static void CompleteRead(WDFREQUEST Read, const REPORT *Report)
{
    NTSTATUS status = CopyToRequest(Read, Report->Data, Report->Length);
    WdfRequestComplete(Read, status);
}

/* IOCTL_HID_READ_REPORT: answer from the backlog, or wait for the agent's next report. */
static void ReadReport(WDFDEVICE Device, WDFREQUEST Request)
{
    DEVICE_CONTEXT *context = GetContext(Device);
    if (context->BacklogCount > 0) {
        CompleteRead(Request, &context->Backlog[context->BacklogStart]);
        context->BacklogStart = (context->BacklogStart + 1) % BACKLOG;
        context->BacklogCount--;
        return;
    }
    NTSTATUS status = WdfRequestForwardToIoQueue(Request, context->ReadQueue);
    if (!NT_SUCCESS(status)) {
        WdfRequestComplete(Request, status);
    }
}

/*
 * A feed report from the agent (IOCTL_HID_WRITE_REPORT or IOCTL_UMDF_HID_SET_OUTPUT_REPORT).
 * UMDF passes the report, ID byte first, in the input buffer.
 */
static NTSTATUS Feed(WDFDEVICE Device, WDFREQUEST Request)
{
    DEVICE_CONTEXT *context = GetContext(Device);
    WDFMEMORY memory;
    size_t length;
    NTSTATUS status = WdfRequestRetrieveInputMemory(Request, &memory);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    const UCHAR *buffer = WdfMemoryGetBuffer(memory, &length);
    if (length < 1 + FEED_LEN || buffer[0] != REPORTID_FEED) {
        return STATUS_INVALID_PARAMETER;
    }
    REPORT report;
    report.Length = InputReportLength(buffer[1]);
    if (report.Length == 0) {
        return STATUS_INVALID_PARAMETER;
    }
    RtlCopyMemory(report.Data, buffer + 1, report.Length);

    WDFREQUEST read;
    if (NT_SUCCESS(WdfIoQueueRetrieveNextRequest(context->ReadQueue, &read))) {
        CompleteRead(read, &report);
    } else {
        if (context->BacklogCount == BACKLOG) {
            /* HIDCLASS isn't reading (nobody has the touchpad open); keep the newest. */
            context->BacklogStart = (context->BacklogStart + 1) % BACKLOG;
            context->BacklogCount--;
        }
        context->Backlog[(context->BacklogStart + context->BacklogCount) % BACKLOG] = report;
        context->BacklogCount++;
    }
    WdfRequestSetInformation(Request, 1 + FEED_LEN);
    return STATUS_SUCCESS;
}

/* IOCTL_UMDF_HID_GET_FEATURE: the report ID is in the input buffer, the report goes out. */
static NTSTATUS GetFeature(WDFDEVICE Device, WDFREQUEST Request)
{
    DEVICE_CONTEXT *context = GetContext(Device);
    WDFMEMORY memory;
    size_t length;
    NTSTATUS status = WdfRequestRetrieveInputMemory(Request, &memory);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    const UCHAR *input = WdfMemoryGetBuffer(memory, &length);
    if (length < 1) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    UCHAR report[1 + sizeof(CertificationBlob)];
    report[0] = input[0];
    switch (input[0]) {
    case REPORTID_CAPS:
        /* Low nibble: contact count maximum; high nibble: pad type 0, a click pad. */
        report[1] = CONTACTS;
        return CopyToRequest(Request, report, 2);
    case REPORTID_PTPHQA:
        RtlCopyMemory(report + 1, CertificationBlob, sizeof(CertificationBlob));
        return CopyToRequest(Request, report, sizeof(report));
    case REPORTID_INPUT_MODE:
        report[1] = context->InputMode;
        return CopyToRequest(Request, report, 2);
    case REPORTID_FUNCTION_SWITCH:
        report[1] = context->FunctionSwitch;
        return CopyToRequest(Request, report, 2);
    default:
        return STATUS_INVALID_PARAMETER;
    }
}

/* IOCTL_UMDF_HID_SET_FEATURE: the report, ID byte first, is in the input buffer. */
static NTSTATUS SetFeature(WDFDEVICE Device, WDFREQUEST Request)
{
    DEVICE_CONTEXT *context = GetContext(Device);
    WDFMEMORY memory;
    size_t length;
    NTSTATUS status = WdfRequestRetrieveInputMemory(Request, &memory);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    const UCHAR *input = WdfMemoryGetBuffer(memory, &length);
    if (length < 2) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    /*
     * Windows sets input mode 3 (touchpad) when it takes the device over. The agent always
     * sends touchpad reports, so the mode is only remembered for GET_FEATURE.
     */
    switch (input[0]) {
    case REPORTID_INPUT_MODE:
        context->InputMode = input[1];
        break;
    case REPORTID_FUNCTION_SWITCH:
        context->FunctionSwitch = input[1];
        break;
    default:
        return STATUS_INVALID_PARAMETER;
    }
    WdfRequestSetInformation(Request, length);
    return STATUS_SUCCESS;
}

/* IOCTL_HID_GET_STRING: UMDF passes the string ID (low word) in the input buffer. */
static NTSTATUS GetString(WDFREQUEST Request)
{
    WDFMEMORY memory;
    size_t length;
    NTSTATUS status = WdfRequestRetrieveInputMemory(Request, &memory);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    const ULONG *input = WdfMemoryGetBuffer(memory, &length);
    if (length < sizeof(ULONG)) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    switch (*input & 0xffff) {
    case HID_STRING_ID_IMANUFACTURER:
        return CopyToRequest(Request, Manufacturer, sizeof(Manufacturer));
    case HID_STRING_ID_IPRODUCT:
        return CopyToRequest(Request, Product, sizeof(Product));
    case HID_STRING_ID_ISERIALNUMBER:
        return CopyToRequest(Request, SerialNumber, sizeof(SerialNumber));
    default:
        return STATUS_INVALID_PARAMETER;
    }
}

void EvtIoDeviceControl(
    WDFQUEUE Queue,
    WDFREQUEST Request,
    size_t OutputBufferLength,
    size_t InputBufferLength,
    ULONG IoControlCode)
{
    UNREFERENCED_PARAMETER(OutputBufferLength);
    UNREFERENCED_PARAMETER(InputBufferLength);
    WDFDEVICE device = WdfIoQueueGetDevice(Queue);
    NTSTATUS status;

    switch (IoControlCode) {
    case IOCTL_HID_GET_DEVICE_DESCRIPTOR:
        status = CopyToRequest(Request, &HidDescriptor, sizeof(HidDescriptor));
        break;
    case IOCTL_HID_GET_REPORT_DESCRIPTOR:
        status = CopyToRequest(Request, ReportDescriptor, sizeof(ReportDescriptor));
        break;
    case IOCTL_HID_GET_DEVICE_ATTRIBUTES: {
        HID_DEVICE_ATTRIBUTES attributes = {0};
        attributes.Size = sizeof(attributes);
        attributes.VendorID = VENDOR_ID;
        attributes.ProductID = PRODUCT_ID;
        attributes.VersionNumber = VERSION;
        status = CopyToRequest(Request, &attributes, sizeof(attributes));
        break;
    }
    case IOCTL_HID_READ_REPORT:
        ReadReport(device, Request);
        return;
    case IOCTL_HID_WRITE_REPORT:
    case IOCTL_UMDF_HID_SET_OUTPUT_REPORT:
        status = Feed(device, Request);
        break;
    case IOCTL_UMDF_HID_GET_FEATURE:
        status = GetFeature(device, Request);
        break;
    case IOCTL_UMDF_HID_SET_FEATURE:
        status = SetFeature(device, Request);
        break;
    case IOCTL_HID_GET_STRING:
        status = GetString(Request);
        break;
    case IOCTL_HID_ACTIVATE_DEVICE:
    case IOCTL_HID_DEACTIVATE_DEVICE:
        status = STATUS_SUCCESS;
        break;
    default:
        status = STATUS_NOT_IMPLEMENTED;
        break;
    }
    WdfRequestComplete(Request, status);
}

NTSTATUS EvtDeviceAdd(WDFDRIVER Driver, PWDFDEVICE_INIT DeviceInit)
{
    UNREFERENCED_PARAMETER(Driver);

    /* mshidumdf is the function driver; this driver sits below it and owns no power policy. */
    WdfFdoInitSetFilter(DeviceInit);

    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, DEVICE_CONTEXT);
    WDFDEVICE device;
    NTSTATUS status = WdfDeviceCreate(&DeviceInit, &attributes, &device);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    DEVICE_CONTEXT *context = GetContext(device);
    context->InputMode = 3;
    context->FunctionSwitch = 0x03;

    /* Sequential, so the backlog is only ever touched by one request at a time. */
    WDF_IO_QUEUE_CONFIG config;
    WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&config, WdfIoQueueDispatchSequential);
    config.EvtIoDeviceControl = EvtIoDeviceControl;
    WDFQUEUE queue;
    status = WdfIoQueueCreate(device, &config, WDF_NO_OBJECT_ATTRIBUTES, &queue);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    WDF_IO_QUEUE_CONFIG_INIT(&config, WdfIoQueueDispatchManual);
    config.PowerManaged = WdfFalse;
    return WdfIoQueueCreate(device, &config, WDF_NO_OBJECT_ATTRIBUTES, &context->ReadQueue);
}

NTSTATUS DriverEntry(PDRIVER_OBJECT DriverObject, PUNICODE_STRING RegistryPath)
{
    WDF_DRIVER_CONFIG config;
    WDF_DRIVER_CONFIG_INIT(&config, EvtDeviceAdd);
    return WdfDriverCreate(DriverObject, RegistryPath, WDF_NO_OBJECT_ATTRIBUTES, &config,
                           WDF_NO_HANDLE);
}
