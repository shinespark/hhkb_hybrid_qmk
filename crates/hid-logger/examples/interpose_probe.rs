use std::ffi::c_void;
use std::process::ExitCode;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDDeviceSetReport(
        device: *mut c_void,
        report_type: u32,
        report_id: isize,
        report: *const u8,
        length: isize,
    ) -> i32;
}

fn main() -> ExitCode {
    // ロガーなしで実行すると、実物の IOKit に NULL デバイスを渡してしまう
    if std::env::var_os("DYLD_INSERT_LIBRARIES").is_none() {
        eprintln!("DYLD_INSERT_LIBRARIES is required");
        return ExitCode::from(2);
    }
    let report = [0x01u8, 0x02, 0x03, 0x04];
    let result = unsafe {
        IOHIDDeviceSetReport(
            std::ptr::null_mut(),
            1,
            0,
            report.as_ptr(),
            report.len() as isize,
        )
    };
    println!("returned {result}");
    ExitCode::SUCCESS
}
