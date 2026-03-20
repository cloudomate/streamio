//! Streamio Virtual Display Controller
//!
//! CLI tool to create/destroy/list virtual displays via the
//! Streamio IddCx display driver.
//!
//! Usage:
//!   display-ctl create 1920 1080 60    # Create a 1920x1080@60Hz display
//!   display-ctl destroy 0              # Destroy display #0
//!   display-ctl list                   # List all virtual displays

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        print_usage();
        std::process::exit(1);
    }

    #[cfg(not(windows))]
    {
        eprintln!("This tool only works on Windows.");
        std::process::exit(1);
    }

    #[cfg(windows)]
    match args[1].as_str() {
        "create" => {
            if args.len() < 5 {
                eprintln!("Usage: display-ctl create <width> <height> <refresh_hz>");
                std::process::exit(1);
            }
            let width: u32 = args[2].parse().expect("Invalid width");
            let height: u32 = args[3].parse().expect("Invalid height");
            let refresh: u32 = args[4].parse().expect("Invalid refresh rate");
            match win::create_display(width, height, refresh) {
                Ok(id) => println!("Created virtual display #{} ({}x{}@{}Hz)", id, width, height, refresh),
                Err(e) => {
                    eprintln!("Failed to create display: {}", e);
                    std::process::exit(1);
                }
            }
        }
        "destroy" => {
            if args.len() < 3 {
                eprintln!("Usage: display-ctl destroy <display_id>");
                std::process::exit(1);
            }
            let id: u32 = args[2].parse().expect("Invalid display ID");
            match win::destroy_display(id) {
                Ok(()) => println!("Destroyed virtual display #{}", id),
                Err(e) => {
                    eprintln!("Failed to destroy display: {}", e);
                    std::process::exit(1);
                }
            }
        }
        "list" => {
            match win::list_displays() {
                Ok(displays) => {
                    if displays.is_empty() {
                        println!("No virtual displays active.");
                    } else {
                        println!("{:<4} {:<12} {:<10} {}", "ID", "Resolution", "Refresh", "Status");
                        println!("{}", "-".repeat(40));
                        for d in &displays {
                            println!("{:<4} {}x{:<8} {}Hz{:<6} {}",
                                d.display_id, d.width, d.height, d.refresh_hz, "",
                                if d.active { "active" } else { "inactive" });
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Failed to list displays: {}", e);
                    std::process::exit(1);
                }
            }
        }
        _ => {
            print_usage();
            std::process::exit(1);
        }
    }
}

fn print_usage() {
    eprintln!("Streamio Virtual Display Controller");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  display-ctl create <width> <height> <refresh_hz>");
    eprintln!("  display-ctl destroy <display_id>");
    eprintln!("  display-ctl list");
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::io;

    type HANDLE = *mut c_void;
    const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;

    // Must match streamio-display.h
    const FILE_DEVICE_STREAMIO_DISPLAY: u32 = 0x8001;
    const METHOD_BUFFERED: u32 = 0;
    const FILE_ANY_ACCESS: u32 = 0;

    const fn ctl_code(device: u32, function: u32, method: u32, access: u32) -> u32 {
        (device << 16) | (access << 14) | (function << 2) | method
    }

    const IOCTL_DISPLAY_CREATE: u32 = ctl_code(FILE_DEVICE_STREAMIO_DISPLAY, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS);
    const IOCTL_DISPLAY_DESTROY: u32 = ctl_code(FILE_DEVICE_STREAMIO_DISPLAY, 0x801, METHOD_BUFFERED, FILE_ANY_ACCESS);
    const IOCTL_DISPLAY_LIST: u32 = ctl_code(FILE_DEVICE_STREAMIO_DISPLAY, 0x802, METHOD_BUFFERED, FILE_ANY_ACCESS);

    const STREAMIO_MAX_DISPLAYS: usize = 16;

    #[repr(C)]
    struct CreateRequest {
        width: u32,
        height: u32,
        refresh_hz: u32,
    }

    #[repr(C)]
    struct CreateResponse {
        display_id: u32,
        status: u32,
    }

    #[repr(C)]
    struct DestroyRequest {
        display_id: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct DisplayInfo {
        display_id: u32,
        width: u32,
        height: u32,
        refresh_hz: u32,
        active: u32,
    }

    #[repr(C)]
    struct ListResponse {
        count: u32,
        displays: [DisplayInfo; STREAMIO_MAX_DISPLAYS],
    }

    pub struct DisplayData {
        pub display_id: u32,
        pub width: u32,
        pub height: u32,
        pub refresh_hz: u32,
        pub active: bool,
    }

    // SetupDi types
    #[repr(C)]
    struct SP_DEVICE_INTERFACE_DATA {
        cb_size: u32,
        interface_class_guid: [u8; 16],
        flags: u32,
        reserved: usize,
    }

    #[repr(C)]
    struct SP_DEVICE_INTERFACE_DETAIL_DATA_W {
        cb_size: u32,
        device_path: [u16; 1],
    }

    type HDEVINFO = *mut c_void;
    const DIGCF_PRESENT: u32 = 0x02;
    const DIGCF_DEVICEINTERFACE: u32 = 0x10;
    const GENERIC_READ: u32 = 0x80000000;
    const GENERIC_WRITE: u32 = 0x40000000;
    const OPEN_EXISTING: u32 = 3;

    #[link(name = "setupapi")]
    extern "system" {
        fn SetupDiGetClassDevsW(
            class_guid: *const [u8; 16], enumerator: *const u16,
            hwnd_parent: HANDLE, flags: u32,
        ) -> HDEVINFO;
        fn SetupDiEnumDeviceInterfaces(
            dev_info: HDEVINFO, dev_info_data: *mut c_void,
            interface_class_guid: *const [u8; 16], member_index: u32,
            device_interface_data: *mut SP_DEVICE_INTERFACE_DATA,
        ) -> i32;
        fn SetupDiGetDeviceInterfaceDetailW(
            dev_info: HDEVINFO,
            device_interface_data: *mut SP_DEVICE_INTERFACE_DATA,
            detail_data: *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W,
            detail_data_size: u32,
            required_size: *mut u32,
            device_info_data: *mut c_void,
        ) -> i32;
        fn SetupDiDestroyDeviceInfoList(dev_info: HDEVINFO) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            name: *const u16, access: u32, share: u32,
            security: *mut c_void, disposition: u32,
            flags: u32, template: HANDLE,
        ) -> HANDLE;
        fn DeviceIoControl(
            device: HANDLE, control_code: u32,
            in_buffer: *const c_void, in_size: u32,
            out_buffer: *mut c_void, out_size: u32,
            bytes_returned: *mut u32, overlapped: *mut c_void,
        ) -> i32;
        fn CloseHandle(handle: HANDLE) -> i32;
        fn GetLastError() -> u32;
    }

    // GUID_DEVINTERFACE_STREAMIO_DISPLAY = {B7E3D5A2-4F1C-8E6D-A9C0-2B5D7F0E3A1C}
    const GUID_BYTES: [u8; 16] = [
        0xA2, 0xD5, 0xE3, 0xB7,  // Data1 LE
        0x1C, 0x4F,              // Data2 LE
        0x6D, 0x8E,              // Data3 LE
        0xA9, 0xC0, 0x2B, 0x5D, 0x7F, 0x0E, 0x3A, 0x1C,
    ];

    fn open_device() -> Result<HANDLE, String> {
        unsafe {
            let dev_info = SetupDiGetClassDevsW(
                &GUID_BYTES,
                std::ptr::null(),
                std::ptr::null_mut(),
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            );
            if dev_info == INVALID_HANDLE_VALUE || dev_info.is_null() {
                return Err(format!("SetupDiGetClassDevsW failed: {}", GetLastError()));
            }

            let mut iface_data: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
            iface_data.cb_size = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;

            if SetupDiEnumDeviceInterfaces(
                dev_info, std::ptr::null_mut(), &GUID_BYTES, 0, &mut iface_data,
            ) == 0 {
                let err = GetLastError();
                SetupDiDestroyDeviceInfoList(dev_info);
                return Err(format!(
                    "Streamio display driver not found (err={}). Is it installed?", err
                ));
            }

            let mut required_size = 0u32;
            SetupDiGetDeviceInterfaceDetailW(
                dev_info, &mut iface_data, std::ptr::null_mut(), 0,
                &mut required_size, std::ptr::null_mut(),
            );

            let mut detail_buf = vec![0u8; required_size as usize];
            let detail = detail_buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cb_size = 8; // 64-bit

            if SetupDiGetDeviceInterfaceDetailW(
                dev_info, &mut iface_data, detail, required_size,
                std::ptr::null_mut(), std::ptr::null_mut(),
            ) == 0 {
                let err = GetLastError();
                SetupDiDestroyDeviceInfoList(dev_info);
                return Err(format!("GetDeviceInterfaceDetail failed: {}", err));
            }

            let path_ptr = &(*detail).device_path as *const u16;
            let path_len = (required_size as usize - 4) / 2;
            let path_slice = std::slice::from_raw_parts(path_ptr, path_len);
            let path_end = path_slice.iter().position(|&c| c == 0).unwrap_or(path_len);
            let _path = String::from_utf16_lossy(&path_slice[..path_end]);

            SetupDiDestroyDeviceInfoList(dev_info);

            let mut path_wide: Vec<u16> = _path.encode_utf16().collect();
            path_wide.push(0);

            let handle = CreateFileW(
                path_wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );

            if handle == INVALID_HANDLE_VALUE {
                return Err(format!("CreateFile failed: {}", GetLastError()));
            }

            Ok(handle)
        }
    }

    pub fn create_display(width: u32, height: u32, refresh_hz: u32) -> Result<u32, String> {
        let handle = open_device()?;
        let req = CreateRequest { width, height, refresh_hz };
        let mut resp: CreateResponse = unsafe { std::mem::zeroed() };
        let mut returned = 0u32;

        let ok = unsafe {
            DeviceIoControl(
                handle, IOCTL_DISPLAY_CREATE,
                &req as *const _ as *const c_void,
                std::mem::size_of::<CreateRequest>() as u32,
                &mut resp as *mut _ as *mut c_void,
                std::mem::size_of::<CreateResponse>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        unsafe { CloseHandle(handle); }

        if ok == 0 {
            return Err(format!("IOCTL_DISPLAY_CREATE failed: {}", unsafe { GetLastError() }));
        }
        if resp.status != 0 {
            return Err(format!("Driver returned error status: {}", resp.status));
        }
        Ok(resp.display_id)
    }

    pub fn destroy_display(display_id: u32) -> Result<(), String> {
        let handle = open_device()?;
        let req = DestroyRequest { display_id };
        let mut returned = 0u32;

        let ok = unsafe {
            DeviceIoControl(
                handle, IOCTL_DISPLAY_DESTROY,
                &req as *const _ as *const c_void,
                std::mem::size_of::<DestroyRequest>() as u32,
                std::ptr::null_mut(), 0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        unsafe { CloseHandle(handle); }

        if ok == 0 {
            return Err(format!("IOCTL_DISPLAY_DESTROY failed: {}", unsafe { GetLastError() }));
        }
        Ok(())
    }

    pub fn list_displays() -> Result<Vec<DisplayData>, String> {
        let handle = open_device()?;
        let mut resp: ListResponse = unsafe { std::mem::zeroed() };
        let mut returned = 0u32;

        let ok = unsafe {
            DeviceIoControl(
                handle, IOCTL_DISPLAY_LIST,
                std::ptr::null(), 0,
                &mut resp as *mut _ as *mut c_void,
                std::mem::size_of::<ListResponse>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        unsafe { CloseHandle(handle); }

        if ok == 0 {
            return Err(format!("IOCTL_DISPLAY_LIST failed: {}", unsafe { GetLastError() }));
        }

        let mut result = Vec::new();
        for i in 0..resp.count as usize {
            let d = &resp.displays[i];
            result.push(DisplayData {
                display_id: d.display_id,
                width: d.width,
                height: d.height,
                refresh_hz: d.refresh_hz,
                active: d.active != 0,
            });
        }
        Ok(result)
    }
}
