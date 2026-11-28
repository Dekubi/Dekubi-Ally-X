// Ported from Dekubi Ally (github.com/Dekubi/Dekubi-Ally, Fritchii branch
// compute-stalls, src/yuzu/dekubi/scskiller.cpp), GPL-3.0-or-later, by Dekubi.
//! SCSKiller (Shader Compilation Stutter Killer), pre-launch part: runs
//! before the title starts unless `DEKUBI_SCS_AUTO=0`.
//!
//! 1. Notes a GPU driver change since the last launch. Nothing is deleted:
//!    the DXIL cache does not depend on the driver, and the host refuses a
//!    pipeline library made by another driver, so it is made again.
//! 2. NVIDIA: raises the driver's own shader cache (where compiled
//!    pipelines live between runs) from the 4 GB default to 10 GB. Needs
//!    administrator rights; without them it is skipped quietly.
//! 3. Keeps Windows Disk Cleanup from deleting compiled shaders
//!    (`D3D Shader Cache` Autorun = 0, the old value kept as
//!    `SCSKillerAutorun`). Also needs administrator rights.

use std::path::Path;

/// Runs the pre-launch checks; never fails the launch.
pub fn run_on_launch(cache_dir: &Path) {
    if std::env::var("DEKUBI_SCS_AUTO").is_ok_and(|v| v.trim() == "0") {
        tracing::info!("SCSKiller skipped (DEKUBI_SCS_AUTO=0)");
        return;
    }
    #[cfg(windows)]
    imp::run(cache_dir);
    #[cfg(not(windows))]
    let _ = cache_dir;
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS,
    };
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_DWORD, RRF_RT_REG_DWORD, RegCloseKey,
        RegGetValueA, RegOpenKeyExA, RegSetValueExA,
    };

    const ID_INITIALIZE: u32 = 0x0150_E828;
    const ID_DRIVER_VERSION: u32 = 0x2926_AAAD;
    const ID_DRS_CREATE: u32 = 0x0694_D52E;
    const ID_DRS_DESTROY: u32 = 0xDAD9_CFF8;
    const ID_DRS_LOAD: u32 = 0x375D_BD6B;
    const ID_DRS_SAVE: u32 = 0xFCBC_7E14;
    const ID_DRS_BASE_PROFILE: u32 = 0xDA84_66A0;
    const ID_DRS_GET: u32 = 0x73BF_8338;
    const ID_DRS_SET: u32 = 0x577D_D202;

    const SHADER_CACHE_SIZE: u32 = 0x00AC_8497;
    const UNLIMITED: u32 = 0xFFFF_FFFF;
    const WANT_MIB: u32 = 10_240;
    const SETTING_SIZE: usize = 0x3020;
    const SETTING_VERSION1: u32 = 0x0001_3020;
    const OFF_ID: usize = 0x1004;
    const OFF_TYPE: usize = 0x1008;
    const OFF_PREDEFINED: usize = 0x1010;
    const OFF_VALUE: usize = 0x201C;
    const NVAPI_OK: i32 = 0;
    const NVAPI_SETTING_NOT_FOUND: i32 = -160;
    const NVAPI_INVALID_USER_PRIVILEGE: i32 = -137;

    const CLEANUP_KEY: &[u8] =
        b"SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\VolumeCaches\\D3D Shader Cache\0";

    type Query = unsafe extern "C" fn(u32) -> *mut c_void;

    fn nvapi() -> Option<Query> {
        static Q: std::sync::OnceLock<Option<Query>> = std::sync::OnceLock::new();
        *Q.get_or_init(|| {
            let name: Vec<u16> = "nvapi64.dll\0".encode_utf16().collect();
            // SAFETY: a NUL-terminated name; a null module means no NVIDIA driver.
            let module = unsafe { LoadLibraryW(name.as_ptr()) };
            if module.is_null() {
                return None;
            }
            // SAFETY: a loaded module and a NUL-terminated export name.
            let proc = unsafe { GetProcAddress(module, c"nvapi_QueryInterface".as_ptr().cast()) }?;
            // SAFETY: nvapi_QueryInterface has this signature.
            let query: Query = unsafe { std::mem::transmute(proc) };
            if let Some(init) = get::<unsafe extern "C" fn() -> i32>(query, ID_INITIALIZE) {
                // SAFETY: NvAPI_Initialize takes nothing.
                unsafe { init() };
            }
            Some(query)
        })
    }

    fn get<F: Copy>(query: Query, id: u32) -> Option<F> {
        // SAFETY: the interface id names a function of type `F`.
        let p = unsafe { query(id) };
        (!p.is_null()).then(|| unsafe { std::mem::transmute_copy::<*mut c_void, F>(&p) })
    }

    fn driver_version() -> Option<String> {
        let f: unsafe extern "C" fn(*mut u32, *mut u8) -> i32 = get(nvapi()?, ID_DRIVER_VERSION)?;
        let mut version = 0u32;
        let mut branch = [0u8; 64];
        // SAFETY: out parameters of the sizes NVAPI writes.
        (unsafe { f(&mut version, branch.as_mut_ptr()) } == NVAPI_OK && version > 0)
            .then(|| format!("{}.{:02}", version / 100, version % 100))
    }

    pub(super) fn run(cache_dir: &Path) {
        let driver = driver_version();
        check_driver_change(cache_dir, driver.as_deref());
        if driver.is_some() {
            match raise_nvidia_cache() {
                Ok(msg) => tracing::info!("SCSKiller: {msg}"),
                Err((msg, true)) => tracing::info!("SCSKiller: {msg} (needs administrator)"),
                Err((msg, false)) => tracing::warn!("SCSKiller: {msg}"),
            }
        }
        match protect_from_cleanup() {
            Ok(msg) => tracing::info!("SCSKiller: {msg}"),
            Err((msg, admin)) => tracing::info!(
                "SCSKiller: Disk Cleanup protection skipped{}: {msg}",
                if admin { " (needs administrator)" } else { "" }
            ),
        }
        tracing::info!(
            driver = driver.as_deref().unwrap_or("not NVIDIA"),
            "SCSKiller: pre-launch checks complete"
        );
    }

    fn check_driver_change(cache_dir: &Path, driver: Option<&str>) {
        let Some(driver) = driver else { return };
        let stamp = cache_dir.join("scskiller_driver.txt");
        let previous = std::fs::read_to_string(&stamp).unwrap_or_default();
        let previous = previous.trim();
        if !previous.is_empty() && previous != driver {
            tracing::info!(
                previous,
                driver,
                "SCSKiller: GPU driver changed; the pipeline library is made again, the DXIL cache is kept"
            );
        }
        let _ = std::fs::create_dir_all(cache_dir);
        let _ = std::fs::write(&stamp, format!("{driver}\n"));
    }

    type Msg = (String, bool);

    fn raise_nvidia_cache() -> Result<String, Msg> {
        let q = nvapi().ok_or(("NVAPI not available".to_owned(), false))?;
        let missing = || ("NVAPI DRS functions not available".to_owned(), false);
        let create: unsafe extern "C" fn(*mut *mut c_void) -> i32 =
            get(q, ID_DRS_CREATE).ok_or_else(missing)?;
        let destroy: unsafe extern "C" fn(*mut c_void) -> i32 =
            get(q, ID_DRS_DESTROY).ok_or_else(missing)?;
        let load: unsafe extern "C" fn(*mut c_void) -> i32 =
            get(q, ID_DRS_LOAD).ok_or_else(missing)?;
        let save: unsafe extern "C" fn(*mut c_void) -> i32 =
            get(q, ID_DRS_SAVE).ok_or_else(missing)?;
        let base: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> i32 =
            get(q, ID_DRS_BASE_PROFILE).ok_or_else(missing)?;
        let get_setting: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut u8) -> i32 =
            get(q, ID_DRS_GET).ok_or_else(missing)?;
        let set_setting: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut u8) -> i32 =
            get(q, ID_DRS_SET).ok_or_else(missing)?;

        let mut session = std::ptr::null_mut();
        // SAFETY: NVAPI DRS calls with the out parameters they write.
        unsafe {
            if create(&mut session) != NVAPI_OK || session.is_null() {
                return Err(("could not open an NVAPI DRS session".to_owned(), false));
            }
            struct Guard(*mut c_void, unsafe extern "C" fn(*mut c_void) -> i32);
            impl Drop for Guard {
                fn drop(&mut self) {
                    // SAFETY: the session this guard was made for.
                    unsafe { (self.1)(self.0) };
                }
            }
            let _guard = Guard(session, destroy);
            if load(session) != NVAPI_OK {
                return Err(("could not load NVIDIA DRS settings".to_owned(), false));
            }
            let mut profile = std::ptr::null_mut();
            if base(session, &mut profile) != NVAPI_OK || profile.is_null() {
                return Err(("could not open the NVIDIA base profile".to_owned(), false));
            }
            let mut buf = vec![0u8; SETTING_SIZE];
            buf[..4].copy_from_slice(&SETTING_VERSION1.to_le_bytes());
            let status = get_setting(session, profile, SHADER_CACHE_SIZE, buf.as_mut_ptr());
            let dword = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
            if status == NVAPI_OK {
                let value = dword(&buf, OFF_VALUE);
                let predefined = dword(&buf, OFF_PREDEFINED) != 0;
                if value == UNLIMITED || (!predefined && value >= WANT_MIB) {
                    return Ok(format!(
                        "NVIDIA driver shader cache already {}",
                        if value == UNLIMITED {
                            "unlimited".to_owned()
                        } else {
                            format!("{} GB", value / 1024)
                        }
                    ));
                }
            } else if status != NVAPI_SETTING_NOT_FOUND {
                return Err((
                    format!("could not read the NVIDIA shader cache size ({status})"),
                    false,
                ));
            }
            let mut buf = vec![0u8; SETTING_SIZE];
            buf[..4].copy_from_slice(&SETTING_VERSION1.to_le_bytes());
            buf[OFF_ID..OFF_ID + 4].copy_from_slice(&SHADER_CACHE_SIZE.to_le_bytes());
            buf[OFF_TYPE..OFF_TYPE + 4].copy_from_slice(&0u32.to_le_bytes());
            buf[OFF_VALUE..OFF_VALUE + 4].copy_from_slice(&WANT_MIB.to_le_bytes());
            let set = set_setting(session, profile, buf.as_mut_ptr());
            if set != NVAPI_OK {
                return Err((
                    format!("NVIDIA shader cache not raised ({set})"),
                    set == NVAPI_INVALID_USER_PRIVILEGE,
                ));
            }
            let saved = save(session);
            if saved != NVAPI_OK {
                return Err((
                    format!("NVIDIA shader cache not saved ({saved})"),
                    saved == NVAPI_INVALID_USER_PRIVILEGE,
                ));
            }
        }
        Ok("raised the NVIDIA driver shader cache to 10 GB".to_owned())
    }

    fn protect_from_cleanup() -> Result<String, Msg> {
        let mut value = 1u32;
        let mut size = 4u32;
        // SAFETY: a NUL-terminated key and value name; a DWORD out buffer.
        let st = unsafe {
            RegGetValueA(
                HKEY_LOCAL_MACHINE,
                CLEANUP_KEY.as_ptr(),
                c"Autorun".as_ptr().cast(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&raw mut value).cast(),
                &mut size,
            )
        };
        if st == ERROR_FILE_NOT_FOUND || (st == ERROR_SUCCESS && value == 0) {
            return Ok("Disk Cleanup already keeps compiled shaders".to_owned());
        }
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: as above; `key` is closed below.
        let open = unsafe {
            RegOpenKeyExA(
                HKEY_LOCAL_MACHINE,
                CLEANUP_KEY.as_ptr(),
                0,
                KEY_READ | KEY_WRITE,
                &mut key,
            )
        };
        if open == ERROR_ACCESS_DENIED {
            return Err(("Disk Cleanup settings are read-only".to_owned(), true));
        }
        if open != ERROR_SUCCESS || key.is_null() {
            return Err((
                "no D3D Shader Cache entry in Disk Cleanup".to_owned(),
                false,
            ));
        }
        let zero = 0u32;
        // SAFETY: an open key; DWORD values.
        let st = unsafe {
            if value != 0 {
                RegSetValueExA(
                    key,
                    c"SCSKillerAutorun".as_ptr().cast(),
                    0,
                    REG_DWORD,
                    (&raw const value).cast(),
                    4,
                );
            }
            let st = RegSetValueExA(
                key,
                c"Autorun".as_ptr().cast(),
                0,
                REG_DWORD,
                (&raw const zero).cast(),
                4,
            );
            RegCloseKey(key);
            st
        };
        if st != ERROR_SUCCESS {
            return Err((format!("could not write Autorun=0 ({st})"), false));
        }
        Ok("Windows Disk Cleanup no longer deletes compiled shaders".to_owned())
    }
}
