use std::io;

#[cfg(target_os = "linux")]
pub fn set_subreaper() -> Result<(), io::Error> {
    use libc::PR_SET_CHILD_SUBREAPER;

    let code = unsafe { libc::prctl(PR_SET_CHILD_SUBREAPER, 0, 0, 0) };
    if code != 0 {
        Err(io::Error::from_raw_os_error(code))
    } else {
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
pub fn set_subreaper() -> Result<(), io::Error> {
    Ok(())
}
