#[cfg(target_os = "linux")]
pub(crate) fn process_start(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = stat.rsplit_once(')')?.1;
    Some(fields.split_whitespace().nth(19)?.to_owned())
}

#[cfg(target_os = "macos")]
pub(crate) fn process_start(pid: u32) -> Option<String> {
    let info = process_bsd_info(pid)?;
    Some(format!(
        "{}:{:06}",
        info.pbi_start_tvsec, info.pbi_start_tvusec
    ))
}

#[cfg(target_os = "macos")]
pub(crate) fn process_bsd_info(pid: u32) -> Option<libc::proc_bsdinfo> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: proc_pidinfo writes at most `size` bytes into `info`, and only a
    // return value equal to `size` means the whole struct was filled.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: the call above filled every byte of the struct.
    let info = unsafe { info.assume_init() };
    Some(info)
}
