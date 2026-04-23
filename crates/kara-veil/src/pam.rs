//! Minimal FFI against libpam.so.0 — just enough to authenticate a
//! local user with the `login` service. Intentionally hand-written so
//! kara-veil doesn't need libclang/bindgen at build time (Arch / Artix
//! don't ship those by default).
//!
//! Surface we use:
//!   pam_start(service, user, conv, *handle)    -> int
//!   pam_authenticate(handle, flags)            -> int
//!   pam_end(handle, status)                    -> int
//!
//! Plus the `pam_conv` callback shape so we can return the password when
//! PAM's `PAM_PROMPT_ECHO_OFF` request comes in.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::mem;
use std::ptr;

use zeroize::Zeroize;

#[allow(non_camel_case_types)]
type pam_handle_t = c_void;

const PAM_SUCCESS: c_int = 0;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
#[allow(dead_code)]
const PAM_PROMPT_ECHO_ON: c_int = 2;
#[allow(dead_code)]
const PAM_ERROR_MSG: c_int = 3;
#[allow(dead_code)]
const PAM_TEXT_INFO: c_int = 4;

/// Matches the C layout of `struct pam_message`. Field order and types
/// are fixed by the PAM ABI.
#[repr(C)]
struct pam_message {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct pam_response {
    resp: *mut c_char,
    resp_retcode: c_int,
}

#[repr(C)]
struct pam_conv {
    conv: Option<
        unsafe extern "C" fn(
            num_msg: c_int,
            msg: *mut *const pam_message,
            resp: *mut *mut pam_response,
            appdata_ptr: *mut c_void,
        ) -> c_int,
    >,
    appdata_ptr: *mut c_void,
}

unsafe extern "C" {
    fn pam_start(
        service_name: *const c_char,
        user: *const c_char,
        pam_conversation: *const pam_conv,
        pamh: *mut *mut pam_handle_t,
    ) -> c_int;

    fn pam_end(pamh: *mut pam_handle_t, pam_status: c_int) -> c_int;

    fn pam_authenticate(pamh: *mut pam_handle_t, flags: c_int) -> c_int;
}

/// State handed to the conversation callback so it can answer
/// `PAM_PROMPT_ECHO_OFF` prompts with the user-supplied password.
struct ConvState {
    password: CString,
}

/// Conversation callback. PAM calls this for each prompt in the auth
/// stack; we reply to password prompts with the stored credential and
/// return an empty string for anything else.
unsafe extern "C" fn conv_cb(
    num_msg: c_int,
    msg: *mut *const pam_message,
    resp: *mut *mut pam_response,
    appdata_ptr: *mut c_void,
) -> c_int {
    if num_msg <= 0 || msg.is_null() || resp.is_null() || appdata_ptr.is_null() {
        return 1; // PAM_CONV_ERR
    }
    let state = unsafe { &*(appdata_ptr as *const ConvState) };

    // Allocate reply array with libc::malloc so PAM can free() it.
    let size = (num_msg as usize) * mem::size_of::<pam_response>();
    let replies = unsafe { libc_malloc(size) } as *mut pam_response;
    if replies.is_null() {
        return 5; // PAM_BUF_ERR
    }
    // Zero first so unfilled entries are safe on free.
    unsafe {
        ptr::write_bytes(replies as *mut u8, 0, size);
    }

    for i in 0..num_msg as isize {
        let m = unsafe { *msg.offset(i) };
        if m.is_null() {
            continue;
        }
        let style = unsafe { (*m).msg_style };
        let reply_ptr: *mut c_char = if style == PAM_PROMPT_ECHO_OFF {
            let len = state.password.as_bytes_with_nul().len();
            let buf = unsafe { libc_malloc(len) } as *mut c_char;
            if buf.is_null() {
                continue;
            }
            unsafe {
                ptr::copy_nonoverlapping(
                    state.password.as_ptr(),
                    buf,
                    len,
                );
            }
            buf
        } else {
            // Return empty-string reply for all non-password prompts.
            let buf = unsafe { libc_malloc(1) } as *mut c_char;
            if !buf.is_null() {
                unsafe { *buf = 0 };
            }
            buf
        };
        unsafe {
            (*replies.offset(i)).resp = reply_ptr;
            (*replies.offset(i)).resp_retcode = 0;
        }
    }

    unsafe { *resp = replies };
    PAM_SUCCESS
}

/// Thin wrapper around libc::malloc so we don't pull the libc crate in.
/// PAM frees the buffers we return via plain `free()` — matches here.
unsafe fn libc_malloc(size: usize) -> *mut c_void {
    unsafe extern "C" {
        fn malloc(size: usize) -> *mut c_void;
    }
    unsafe { malloc(size) }
}

/// Authenticate `username` with `password` via the PAM `login` service.
/// Returns `true` on success, `false` on any failure (wrong password,
/// account disabled, PAM error, …).
///
/// The password string is zeroed before this function returns.
pub fn authenticate(username: &str, password: &[u8]) -> bool {
    let Ok(service) = CString::new("login") else {
        return false;
    };
    let Ok(user) = CString::new(username) else {
        return false;
    };
    let Ok(pw) = CString::new(password) else {
        // Password contained a NUL — reject without calling PAM.
        return false;
    };

    let mut state = ConvState { password: pw };
    let conv = pam_conv {
        conv: Some(conv_cb),
        appdata_ptr: &mut state as *mut ConvState as *mut c_void,
    };

    let mut handle: *mut pam_handle_t = ptr::null_mut();
    let start_rc = unsafe {
        pam_start(
            service.as_ptr(),
            user.as_ptr(),
            &conv as *const pam_conv,
            &mut handle as *mut *mut pam_handle_t,
        )
    };
    if start_rc != PAM_SUCCESS || handle.is_null() {
        // Wipe the in-memory copy before bailing.
        zero_cstring(&mut state.password);
        return false;
    }

    let auth_rc = unsafe { pam_authenticate(handle, 0) };

    unsafe { pam_end(handle, auth_rc) };

    // Wipe the credential copy we kept for the callback.
    zero_cstring(&mut state.password);

    auth_rc == PAM_SUCCESS
}

/// Zero a CString's bytes in place. `CString::into_bytes_with_nul` would
/// take ownership; we want to clobber the buffer that's still sitting
/// inside the struct so any lingering copies go away. Cast through the
/// public `as_bytes_with_nul` length to scope the overwrite.
fn zero_cstring(cs: &mut CString) {
    let len = cs.as_bytes_with_nul().len();
    let p = cs.as_ptr() as *mut u8;
    unsafe {
        let slice = std::slice::from_raw_parts_mut(p, len);
        slice.zeroize();
    }
    // Make sure `cs` isn't re-read as a valid C string; re-assign an
    // empty one. The old backing storage has already been zeroed.
    let _ = std::mem::replace(cs, CString::default());
    let _ = CStr::from_bytes_with_nul(b"\0"); // silence unused import
}
