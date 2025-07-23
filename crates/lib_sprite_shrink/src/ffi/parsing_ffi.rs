use std::ptr;
use std::slice;

use crate::lib_structs::{FileHeader};
use crate::parsing::{parse_file_header};

/// Parses a file header from a byte slice.
///
/// On success, returns a pointer to a valid FileHeader.
/// On failure (e.g., invalid magic number, unsupported version), returns a 
/// null pointer.
///
/// # Safety
/// The `header_data_array_ptr` must be a valid pointer to readable memory of
/// at least `header_data_len` bytes. The pointer returned by this function is
/// owned by the caller and MUST be freed by passing it to 
/// `free_file_header_ffi` to prevent memory leaks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn parse_file_header_ffi(
    header_data_array_ptr: *const u8,
    header_data_len: usize
) -> *mut FileHeader {
    /*Prepare header_data, which is the only parameter of the original 
    parse_file_header function, from C input.*/
    let header_data = unsafe {slice::from_raw_parts(
            header_data_array_ptr, 
            header_data_len)
    };

    /*Parse the file header data and return it. On error return null pointer.*/
    match parse_file_header(header_data){
        Ok(data) => {
            Box::into_raw(Box::new(data))
        }
        Err(_) => {
            ptr::null_mut()
        }
    }
}

/// Frees the memory for a FileHeader that was allocated by 
/// `parse_file_header_ffi`.
///
/// # Safety
/// The `ptr` must be a pointer returned from a successful call to
/// `parse_file_header_ffi`. Calling this function with a null pointer or a
/// pointer that has already been freed will lead to undefined behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_file_header_ffi(ptr: *mut FileHeader) {
    unsafe{
        if !ptr.is_null() {
            /*Retake ownership of the pointer from C and drop it, freeing the
            memory.*/
            let _ = Box::from_raw(ptr);
        }
    }
}