use std::io;

pub(crate) fn pthread_error(operation: &str, code: libc::c_int) -> io::Error {
    let error = io::Error::from_raw_os_error(code);
    io::Error::new(error.kind(), format!("{operation} failed: {error}"))
}


pub(crate) fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(crate) fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}