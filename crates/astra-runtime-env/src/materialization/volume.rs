//! Read the filesystem's UUID, never a mount-local device number or mount ID.

use std::{fs::File, io};

pub(super) fn filesystem_uuid(directory: &File) -> io::Result<Vec<u8>> {
    validate_uuid(read_uuid(directory)?)
}

/// Only an unavailable Linux API permits the original device/inode check.
/// Invalid UUID data, permission failures and transient I/O errors are not
/// evidence that the API is unavailable and must keep failing closed.
pub(super) fn uuid_api_unavailable(error: &io::Error) -> bool {
    #[cfg(target_os = "linux")]
    {
        matches!(
            error.raw_os_error(),
            Some(libc::ENOTTY | libc::EOPNOTSUPP | libc::ENOSYS)
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = error;
        false
    }
}

fn validate_uuid(uuid: Vec<u8>) -> io::Result<Vec<u8>> {
    if uuid.is_empty() || uuid.len() > 16 || uuid.iter().all(|byte| *byte == 0) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the filesystem did not provide a nonzero UUID",
        ));
    }
    Ok(uuid)
}

#[cfg(target_os = "macos")]
fn read_uuid(directory: &File) -> io::Result<Vec<u8>> {
    use std::os::fd::AsRawFd;

    let mut attributes = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    // getattrlist packs its length and the requested UUID on 4-byte boundaries.
    #[repr(C)]
    struct VolumeUuid {
        length: u32,
        uuid: [u8; 16],
    }
    let mut output = VolumeUuid {
        length: 0,
        uuid: [0; 16],
    };
    // SAFETY: the live directory fd, initialized attrlist and writable, sized
    // output buffer satisfy fgetattrlist's ABI. Only a fixed-size UUID is read.
    let result = unsafe {
        libc::fgetattrlist(
            directory.as_raw_fd(),
            (&mut attributes as *mut libc::attrlist).cast(),
            (&mut output as *mut VolumeUuid).cast(),
            std::mem::size_of::<VolumeUuid>(),
            0,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if output.length as usize != std::mem::size_of::<VolumeUuid>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "incomplete filesystem UUID response",
        ));
    }
    Ok(output.uuid.to_vec())
}

#[cfg(target_os = "linux")]
fn read_uuid(directory: &File) -> io::Result<Vec<u8>> {
    use std::os::fd::AsRawFd;

    // Linux UAPI: FS_IOC_GETFSUUID = _IOR(0x15, 0, struct fsuuid2).
    // The external UUID is independent of st_dev. An unavailable API is
    // classified above; its caller must retain strict device AND inode checks.
    #[repr(C)]
    struct FsUuid {
        length: u8,
        uuid: [u8; 16],
    }
    let mut output = FsUuid {
        length: 0,
        uuid: [0; 16],
    };
    const GET_FS_UUID: libc::c_ulong = 0x8011_1500;
    // SAFETY: the live fd and writable fsuuid2 buffer match the read-only ioctl
    // ABI on the supported Linux x86_64/aarch64 release targets.
    let result = unsafe { libc::ioctl(directory.as_raw_fd(), GET_FS_UUID as _, &mut output) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    output
        .uuid
        .get(..usize::from(output.length))
        .map(<[u8]>::to_vec)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid filesystem UUID length"))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn read_uuid(_directory: &File) -> io::Result<Vec<u8>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "filesystem UUID lookup is not supported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_invalid_volume_identity_is_not_silently_accepted() {
        for uuid in [vec![], vec![0; 16], vec![1; 17]] {
            assert_eq!(
                validate_uuid(uuid).unwrap_err().kind(),
                io::ErrorKind::Unsupported
            );
        }
        assert_eq!(validate_uuid(vec![1; 16]).unwrap(), vec![1; 16]);
        assert_eq!(validate_uuid(vec![1; 4]).unwrap(), vec![1; 4]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn only_unavailable_linux_api_errors_allow_legacy_validation() {
        for errno in [libc::ENOTTY, libc::EOPNOTSUPP, libc::ENOSYS] {
            assert!(uuid_api_unavailable(&io::Error::from_raw_os_error(errno)));
        }
        for errno in [libc::EACCES, libc::EIO, libc::EINTR, libc::EINVAL] {
            assert!(!uuid_api_unavailable(&io::Error::from_raw_os_error(errno)));
        }
        assert!(!uuid_api_unavailable(
            &validate_uuid(vec![0; 16]).unwrap_err()
        ));
    }
}
