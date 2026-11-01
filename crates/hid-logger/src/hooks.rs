use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;

use crate::session::{DeviceInfo, FileSink, SendOutcome, Session};

type IoHidDeviceRef = *mut c_void;
type IoReturn = i32;
type ReportCallback =
    unsafe extern "C" fn(*mut c_void, IoReturn, *mut c_void, u32, u32, *mut u8, isize);

const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const CF_NUMBER_SINT64_TYPE: isize = 4;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        value: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFStringGetCString(
        value: *const c_void,
        buffer: *mut c_char,
        size: isize,
        encoding: u32,
    ) -> u8;
    fn CFStringGetTypeID() -> usize;
    fn CFNumberGetValue(value: *const c_void, number_type: isize, out: *mut c_void) -> u8;
    fn CFNumberGetTypeID() -> usize;
    fn CFGetTypeID(value: *const c_void) -> usize;
    fn CFRelease(value: *const c_void);
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDDeviceGetProperty(device: IoHidDeviceRef, key: *const c_void) -> *const c_void;
    fn IOHIDDeviceSetReport(
        device: IoHidDeviceRef,
        report_type: u32,
        report_id: isize,
        report: *const u8,
        length: isize,
    ) -> IoReturn;
    fn IOHIDDeviceRegisterInputReportCallback(
        device: IoHidDeviceRef,
        report: *mut u8,
        length: isize,
        callback: Option<ReportCallback>,
        context: *mut c_void,
    );
    fn IOHIDDeviceOpen(device: IoHidDeviceRef, options: u32) -> IoReturn;
    fn IOHIDDeviceClose(device: IoHidDeviceRef, options: u32) -> IoReturn;
}

#[repr(C)]
struct Interpose<T: 'static> {
    replacement: T,
    original: T,
}

unsafe impl<T> Sync for Interpose<T> {}

type SetReportFn = unsafe extern "C" fn(IoHidDeviceRef, u32, isize, *const u8, isize) -> IoReturn;
type RegisterFn =
    unsafe extern "C" fn(IoHidDeviceRef, *mut u8, isize, Option<ReportCallback>, *mut c_void);
type OpenCloseFn = unsafe extern "C" fn(IoHidDeviceRef, u32) -> IoReturn;

#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_SET_REPORT: Interpose<SetReportFn> = Interpose {
    replacement: hook_set_report,
    original: IOHIDDeviceSetReport,
};

#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_REGISTER: Interpose<RegisterFn> = Interpose {
    replacement: hook_register_input,
    original: IOHIDDeviceRegisterInputReportCallback,
};

#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_OPEN: Interpose<OpenCloseFn> = Interpose {
    replacement: hook_open,
    original: IOHIDDeviceOpen,
};

#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_CLOSE: Interpose<OpenCloseFn> = Interpose {
    replacement: hook_close,
    original: IOHIDDeviceClose,
};

static SESSION: OnceLock<Session<FileSink>> = OnceLock::new();

fn session() -> &'static Session<FileSink> {
    SESSION.get_or_init(|| {
        let path = std::env::var_os("HHKB_HID_LOGGER_PATH");
        let mode = std::env::var("HHKB_HID_LOGGER_MODE").ok();
        let offset = std::env::var("HHKB_HID_LOGGER_COMMAND_OFFSET").ok();
        Session::from_values(path.as_deref(), mode.as_deref(), offset.as_deref())
    })
}

extern "C" fn initialize_on_load() {
    let _ = catch_unwind(session);
}

#[used]
#[unsafe(link_section = "__DATA,__mod_init_func")]
static INITIALIZER: extern "C" fn() = initialize_on_load;

unsafe fn cf_key(name: &CStr) -> *const c_void {
    unsafe { CFStringCreateWithCString(std::ptr::null(), name.as_ptr(), CF_STRING_ENCODING_UTF8) }
}

unsafe fn property_number(device: IoHidDeviceRef, name: &CStr) -> Option<i64> {
    unsafe {
        let key = cf_key(name);
        if key.is_null() {
            return None;
        }
        let value = IOHIDDeviceGetProperty(device, key);
        CFRelease(key);
        if value.is_null() || CFGetTypeID(value) != CFNumberGetTypeID() {
            return None;
        }
        let mut number = 0i64;
        let ok = CFNumberGetValue(
            value,
            CF_NUMBER_SINT64_TYPE,
            (&raw mut number).cast::<c_void>(),
        );
        (ok != 0).then_some(number)
    }
}

unsafe fn property_string(device: IoHidDeviceRef, name: &CStr) -> Option<String> {
    unsafe {
        let key = cf_key(name);
        if key.is_null() {
            return None;
        }
        let value = IOHIDDeviceGetProperty(device, key);
        CFRelease(key);
        if value.is_null() || CFGetTypeID(value) != CFStringGetTypeID() {
            return None;
        }
        let mut buffer = [0 as c_char; 128];
        let ok = CFStringGetCString(
            value,
            buffer.as_mut_ptr(),
            buffer.len() as isize,
            CF_STRING_ENCODING_UTF8,
        );
        (ok != 0).then(|| {
            CStr::from_ptr(buffer.as_ptr())
                .to_string_lossy()
                .into_owned()
        })
    }
}

unsafe fn device_info(device: IoHidDeviceRef) -> DeviceInfo {
    if device.is_null() {
        return DeviceInfo::default();
    }
    unsafe {
        DeviceInfo {
            transport: property_string(device, c"Transport"),
            usage_page: property_number(device, c"PrimaryUsagePage"),
            location: property_number(device, c"LocationID"),
        }
    }
}

unsafe fn bytes<'a>(pointer: *const u8, length: isize) -> &'a [u8] {
    if pointer.is_null() || length <= 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(pointer, length as usize) }
    }
}

unsafe extern "C" fn hook_set_report(
    device: IoHidDeviceRef,
    report_type: u32,
    report_id: isize,
    report: *const u8,
    length: isize,
) -> IoReturn {
    let outcome = catch_unwind(AssertUnwindSafe(|| unsafe {
        let info = device_info(device);
        session().send(&info, report_type, report_id, bytes(report, length), || {
            IOHIDDeviceSetReport(device, report_type, report_id, report, length)
        })
    }));
    match outcome {
        Ok(SendOutcome::Forwarded(result)) => result,
        Ok(SendOutcome::Blocked) | Err(_) => {
            eprintln!("hid-logger: 許可されていない送信を止めたため中止します");
            std::process::abort();
        }
    }
}

struct CallbackContext {
    callback: ReportCallback,
    context: *mut c_void,
    device: DeviceInfo,
}

unsafe extern "C" fn input_trampoline(
    context: *mut c_void,
    result: IoReturn,
    sender: *mut c_void,
    report_type: u32,
    report_id: u32,
    report: *mut u8,
    length: isize,
) {
    let state = unsafe { &*context.cast::<CallbackContext>() };
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        session().receive(&state.device, report_type, report_id, bytes(report, length));
    }));
    unsafe {
        (state.callback)(
            state.context,
            result,
            sender,
            report_type,
            report_id,
            report,
            length,
        );
    }
}

unsafe extern "C" fn hook_register_input(
    device: IoHidDeviceRef,
    report: *mut u8,
    length: isize,
    callback: Option<ReportCallback>,
    context: *mut c_void,
) {
    unsafe {
        let Some(callback) = callback else {
            IOHIDDeviceRegisterInputReportCallback(device, report, length, None, context);
            return;
        };
        let info = device_info(device);
        let _ = catch_unwind(AssertUnwindSafe(|| {
            session().device_event("register-input", &info, length as u32, None);
        }));
        let state = Box::into_raw(Box::new(CallbackContext {
            callback,
            context,
            device: info,
        }));
        IOHIDDeviceRegisterInputReportCallback(
            device,
            report,
            length,
            Some(input_trampoline),
            state.cast::<c_void>(),
        );
    }
}

unsafe extern "C" fn hook_open(device: IoHidDeviceRef, options: u32) -> IoReturn {
    unsafe {
        let info = device_info(device);
        let result = IOHIDDeviceOpen(device, options);
        let _ = catch_unwind(AssertUnwindSafe(|| {
            session().device_event("open", &info, options, Some(result));
        }));
        result
    }
}

unsafe extern "C" fn hook_close(device: IoHidDeviceRef, options: u32) -> IoReturn {
    unsafe {
        let info = device_info(device);
        let result = IOHIDDeviceClose(device, options);
        let _ = catch_unwind(AssertUnwindSafe(|| {
            session().device_event("close", &info, options, Some(result));
        }));
        result
    }
}
