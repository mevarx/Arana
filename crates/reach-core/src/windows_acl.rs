//! Owner-only protected DACLs derived from the process token, never environment.
use crate::{ReachError, Result};
use std::{ffi::c_void, os::windows::ffi::OsStrExt, path::Path, ptr};
use windows_sys::Win32::{
    Foundation::{CloseHandle, LocalFree, HANDLE},
    Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    },
    Security::{
        GetTokenInformation, SetFileSecurityW, TokenUser, DACL_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, TOKEN_QUERY, TOKEN_USER,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};
struct Token(HANDLE);
impl Drop for Token {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
struct Local(*mut c_void);
impl Drop for Local {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
fn error() -> ReachError {
    ReachError::new(
        "UPSTREAM_BROKEN",
        format!(
            "Cannot set private state ACL: {}",
            std::io::Error::last_os_error()
        ),
    )
}
pub fn protect(path: &Path, is_dir: bool) -> Result<()> {
    // SAFETY: handles are checked, Win32 output buffers remain alive and aligned
    // until their last use, and every returned LocalAlloc allocation is freed.
    unsafe {
        let mut handle = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) == 0 {
            return Err(error());
        }
        let token = Token(handle);
        let mut needed = 0;
        GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut needed);
        if needed == 0 {
            return Err(error());
        }
        let mut info = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
        if GetTokenInformation(
            token.0,
            TokenUser,
            info.as_mut_ptr().cast(),
            needed,
            &mut needed,
        ) == 0
        {
            return Err(error());
        }
        let user = &*info.as_ptr().cast::<TOKEN_USER>();
        let mut sid_string = ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut sid_string) == 0 {
            return Err(error());
        }
        let _sid_allocation = Local(sid_string.cast());
        let mut length = 0;
        while *sid_string.add(length) != 0 {
            length += 1;
        }
        let sid = String::from_utf16(std::slice::from_raw_parts(sid_string, length))
            .map_err(|_| ReachError::new("UPSTREAM_BROKEN", "Invalid process-token SID"))?;
        let flags = if is_dir { "OICI" } else { "" };
        let sddl = format!("D:P(A;{flags};FA;;;{sid})");
        let sddl: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
        let mut descriptor = ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        ) == 0
        {
            return Err(error());
        }
        let _descriptor_allocation = Local(descriptor);
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        if SetFileSecurityW(
            path.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        ) == 0
        {
            return Err(error());
        }
    }
    Ok(())
}
