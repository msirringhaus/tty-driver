use std::{
    ops::RangeInclusive,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
struct TtyDriver {
    path: PathBuf,
    major: i32,
    minor_range: RangeInclusive<i32>,
}

impl TtyDriver {
    /// Trys to find the TTY for a given process ID.
    /// This is unfortunately not straight forward. We have to do:
    /// 1. Read the tty_nr from /proc/<PID>/stat and do some bit-magic to get major and minor
    /// 2. Read /proc/tty/drivers to see which path corresponds to which major number and minor range
    /// 3. Match those 2 together and find a fitting driver
    /// 4. 'Guess' the resulting path (e.g. either /dev/tty/2 or /dev/tty2)
    /// 5. Verify the guess is correct by stat-ing the result and comparing major and minor
    fn find_tty_for_pid(pid: i32) -> Option<PathBuf> {
        log::info!("Start finding TTY for {pid}");
        // 1. Parse major and minor tty_nr
        let (tty_major, tty_minor) = TtyDriver::get_tty_nr_for_pid(pid)?;
        // 2. Parse /proc/tty/drivers
        let drivers = TtyDriver::parse_tty_drivers();
        // 3. Find a match
        let driver = TtyDriver::match_drivers_to_tty_nr(drivers, tty_major, tty_minor)?;
        // 4. and 5. Guess and verify path
        let path = TtyDriver::guess_tty_path(&driver.path, tty_major, tty_minor)?;
        log::info!("Step 4: {path:?}");

        Some(path)
    }

    fn verify_tty_path(path: &Path, tty_major: i32, tty_minor: i32) -> bool {
        if let Ok(metadata) = path.metadata() {
            let rdev = metadata.rdev() as i32;
            let dev_major = rdev >> 8;
            let dev_minor = rdev & 0xff;
            if dev_major == tty_major && dev_minor == tty_minor {
                return true;
            }
        }
        false
    }

    fn guess_tty_path(path: &Path, tty_major: i32, tty_minor: i32) -> Option<PathBuf> {
        log::info!("Trying to guess the TTY-path");
        // First, guess seperated by slash: (e.g. /dev/tty/2)
        let mut res = path.join(format!("{tty_minor}"));
        log::debug!("Trying {res:?}");
        if res.exists() && TtyDriver::verify_tty_path(&res, tty_major, tty_minor) {
            log::info!("Found and verified {res:?}");
            return Some(res);
        }

        // Otherwise, guess seperated by directly appending the number: (e.g. /dev/tty2)
        let mut second_try = path.as_os_str().to_os_string();
        second_try.push(format!("{tty_minor}"));
        res = PathBuf::from(second_try);
        log::debug!("Trying {res:?}");
        if res.exists() && TtyDriver::verify_tty_path(&res, tty_major, tty_minor) {
            log::info!("Found and verified {res:?}");
            return Some(res);
        }

        // No luck
        None
    }

    fn match_drivers_to_tty_nr(
        drivers: Vec<TtyDriver>,
        tty_major: i32,
        tty_minor: i32,
    ) -> Option<TtyDriver> {
        log::info!("Trying to find a matching driver");

        let driver = drivers
            .into_iter()
            .find(|driver| driver.major == tty_major && driver.minor_range.contains(&tty_minor))?;
        log::info!("Found matching driver: {driver:?}");
        Some(driver)
    }

    fn parse_tty_drivers() -> Vec<TtyDriver> {
        log::info!("Trying to parse TTY-drivers from /proc/tty/drivers");
        let mut drivers = Vec::new();

        // example output:
        // /dev/tty             /dev/tty        5       0 system:/dev/tty
        // /dev/console         /dev/console    5       1 system:console
        // /dev/ptmx            /dev/ptmx       5       2 system
        // /dev/vc/0            /dev/vc/0       4       0 system:vtmaster
        // rfcomm               /dev/rfcomm   216 0-255 serial
        // serial               /dev/ttyS       4 64-95 serial
        // pty_slave            /dev/pts      136 0-1048575 pty:slave
        // pty_master           /dev/ptm      128 0-1048575 pty:master
        // unknown              /dev/tty        4 1-63 console
        let drivers_raw = match std::fs::read_to_string(PathBuf::from("/proc/tty/drivers")) {
            Ok(x) => x,
            Err(_) => {
                return drivers;
            }
        };

        for line in drivers_raw.lines() {
            let parts: Vec<_> = line.split_whitespace().collect();
            if parts.len() < 4 {
                // Something is wrong. Silently ignore this entry
                continue;
            }
            let path = PathBuf::from(parts[1]);
            let major = match parts[2].parse::<i32>() {
                Ok(maj) => maj,
                Err(_) => continue,
            };
            let tty_minor = parts[3];
            let minor_range = match TtyDriver::parse_minor_range(tty_minor) {
                Some(x) => x,
                None => {
                    continue;
                }
            };
            let driver = TtyDriver {
                path,
                major,
                minor_range,
            };
            drivers.push(driver);
        }
        log::info!("Found tty-drivers: {drivers:?}");
        drivers
    }

    // Getting either "3" or "3-10" and parsing a Range from that
    fn parse_minor_range(tty_minor: &str) -> Option<RangeInclusive<i32>> {
        let minor_range: Vec<_> = tty_minor.split('-').collect();
        if minor_range.len() == 1 {
            let start = minor_range[0].parse::<i32>().ok()?;
            Some(start..=start)
        } else if minor_range.len() == 2 {
            let start = minor_range[0].parse::<i32>().ok()?;
            let end = minor_range[1].parse::<i32>().ok()?;
            Some(start..=end)
        } else {
            None
        }
    }

    fn get_tty_nr_for_pid(pid: i32) -> Option<(i32, i32)> {
        if pid == -1 {
            log::info!("Invalid PID");
            return None;
        }
        let procfile = PathBuf::from(format!("/proc/{pid}/stat"));
        let stat = std::fs::read_to_string(&procfile).ok()?;

        let tty_nr = stat
            .split_whitespace()
            .nth(6)
            .and_then(|s| s.parse::<i32>().ok())?;
        // from /usr/include/linux/kdev_t.h
        // #define MAJOR(dev)	((dev)>>8)
        // #define MINOR(dev)	((dev) & 0xff)
        let tty_major = tty_nr >> 8;
        let tty_minor = tty_nr & 0xff;

        log::info!(
            "Got major/minor numbers from {}: tty_major: {tty_major}, tty_minor: {tty_minor}",
            procfile.to_string_lossy()
        );
        Some((tty_major, tty_minor))
    }
}

pub fn find_tty_for_pid(pid: i32) -> Option<PathBuf> {
    TtyDriver::find_tty_for_pid(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }
}
