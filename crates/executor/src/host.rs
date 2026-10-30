// SPDX-FileCopyrightText: 2026 Slavi Pantaleev
//
// SPDX-License-Identifier: Apache-2.0

//! Moving the executor out of its container and onto the host, and switching to the project's users.
//!
//! The container runs privileged with the host's PID namespace, so `/proc/1` is the host's init.
//! Joining its namespaces makes git and the hooks see the host's filesystem, network and tools,
//! while the process stays in the container's cgroup (stopping the container still kills them).
//! The user namespace is not joined: Docker shares the host's unless user remapping is enabled, which is not supported.
//!
//! Credentials are changed with per-thread system calls,
//! which equal per-process ones only while the process has a single thread.
//! Everything here must run before any thread is started.

use std::fs::File;
use std::os::fd::AsFd;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use rustix::process::{Gid, Uid};
use rustix::thread::{self, LinkNameSpaceType};

/// PATH for git and hooks; the container's environment means nothing on the host.
const PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Mount comes last: after it, `/proc` paths would resolve in the host's view.
const NAMESPACES: [(&str, LinkNameSpaceType); 6] = [
    ("cgroup", LinkNameSpaceType::ControlGroup),
    ("ipc", LinkNameSpaceType::InterProcessCommunication),
    ("uts", LinkNameSpaceType::HostNameAndNISDomainName),
    ("net", LinkNameSpaceType::Network),
    ("pid", LinkNameSpaceType::ProcessID),
    ("mnt", LinkNameSpaceType::Mount),
];

/// Joins the namespaces of the host's init process (PID 1, seen through the host's PID namespace).
pub fn enter_host_namespaces() -> Result<()> {
    ensure_single_threaded()?;
    let namespaces = NAMESPACES
        .iter()
        .map(|(name, kind)| {
            let path = format!("/proc/1/ns/{name}");
            let file = File::open(&path).with_context(|| {
                format!("cannot open {path} (is the container privileged, with --pid=host?)")
            })?;
            Ok((path, file, *kind))
        })
        .collect::<Result<Vec<_>>>()?;
    for (path, file, kind) in &namespaces {
        thread::move_into_link_name_space(file.as_fd(), Some(*kind))
            .with_context(|| format!("cannot join {path}"))?;
    }
    std::env::set_current_dir("/").context("cannot change to the host's root directory")?;
    Ok(())
}

/// A user from the host's account files.
/// Only `/etc/passwd` and `/etc/group` are consulted:
/// the static binary has no NSS, so directory-service accounts are not found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostUser {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    /// Supplementary groups, including the primary group.
    pub groups: Vec<u32>,
    pub home: PathBuf,
    pub shell: String,
}

impl HostUser {
    pub fn lookup(name: &str) -> Result<Self> {
        let passwd = std::fs::read_to_string("/etc/passwd").context("cannot read /etc/passwd")?;
        let group = std::fs::read_to_string("/etc/group").context("cannot read /etc/group")?;
        Self::parse(name, &passwd, &group)
    }

    pub fn parse(name: &str, passwd: &str, group: &str) -> Result<Self> {
        let Some(fields) = passwd
            .lines()
            .map(|line| line.split(':').collect::<Vec<_>>())
            .find(|fields| fields.len() == 7 && fields[0] == name)
        else {
            bail!("user {name:?} is not in the host's /etc/passwd");
        };
        let uid = fields[2]
            .parse()
            .with_context(|| format!("user {name:?} has an invalid uid"))?;
        let gid = fields[3]
            .parse()
            .with_context(|| format!("user {name:?} has an invalid gid"))?;
        let mut groups = vec![gid];
        for fields in group
            .lines()
            .map(|line| line.split(':').collect::<Vec<_>>())
        {
            if fields.len() == 4
                && fields[3].split(',').any(|member| member == name)
                && let Ok(member_gid) = fields[2].parse()
                && !groups.contains(&member_gid)
            {
                groups.push(member_gid);
            }
        }
        Ok(Self {
            name: name.to_owned(),
            uid,
            gid,
            groups,
            home: PathBuf::from(fields[5]),
            shell: fields[6].to_owned(),
        })
    }

    /// The whole environment git and the hooks get, like a fresh login's minimal one.
    pub fn environment(&self) -> Vec<(String, String)> {
        vec![
            ("HOME".into(), self.home.display().to_string()),
            ("USER".into(), self.name.clone()),
            ("LOGNAME".into(), self.name.clone()),
            ("SHELL".into(), self.shell.clone()),
            ("PATH".into(), PATH.into()),
        ]
    }

    /// Irreversibly switches the process to this user.
    /// From root to another user, the kernel also clears all capabilities.
    pub fn become_this_user(&self) -> Result<()> {
        ensure_single_threaded()?;
        let groups: Vec<Gid> = self.groups.iter().map(|&g| Gid::from_raw(g)).collect();
        let (uid, gid) = (Uid::from_raw(self.uid), Gid::from_raw(self.gid));
        thread::set_thread_groups(&groups).context("cannot set supplementary groups")?;
        thread::set_thread_res_gid(gid, gid, gid).context("cannot set group id")?;
        thread::set_thread_res_uid(uid, uid, uid).context("cannot set user id")?;
        Ok(())
    }
}

fn ensure_single_threaded() -> Result<()> {
    let status =
        std::fs::read_to_string("/proc/self/status").context("cannot read /proc/self/status")?;
    let threads = status
        .lines()
        .find_map(|line| line.strip_prefix("Threads:"))
        .map(str::trim);
    if threads != Some("1") {
        bail!("refusing to change namespaces or credentials with more than one thread");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/bash\nweb:x:1001:1001:Web:/home/web:/bin/sh\nbroken:x:abc:1:::\n";
    const GROUP: &str = "root:x:0:\nweb:x:1001:\ndocker:x:968:alice,web\nwebsites:x:2000:webmaster\nwheel:x:10:web\n";

    #[test]
    fn finds_the_user_and_its_groups() {
        let user = HostUser::parse("web", PASSWD, GROUP).unwrap();
        assert_eq!((user.uid, user.gid), (1001, 1001));
        assert_eq!(user.groups, vec![1001, 968, 10]);
        assert_eq!(user.home, PathBuf::from("/home/web"));
        assert_eq!(user.shell, "/bin/sh");
    }

    #[test]
    fn rejects_missing_and_broken_users() {
        assert!(HostUser::parse("nobody", PASSWD, GROUP).is_err());
        assert!(HostUser::parse("broken", PASSWD, GROUP).is_err());
    }
}
