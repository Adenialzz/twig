use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub ssh_host: String,
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        let host = &self.ssh_host;
        ensure!(
            !host.is_empty()
                && !host
                    .chars()
                    .any(|c| c.is_whitespace() || c.is_control() || "/?#\\%".contains(c)),
            "[zed].ssh_host 须为本机 SSH 别名或 [user@]host[:port]，不能包含路径"
        );
        let url =
            url::Url::parse(&format!("ssh://{host}")).context("[zed].ssh_host 地址或端口无效")?;
        ensure!(
            url.host_str().is_some() && url.password().is_none(),
            "[zed].ssh_host 必须包含主机，不能包含密码"
        );
        Ok(())
    }

    pub fn link(&self, path: &Path) -> Result<String> {
        self.validate()?;
        ensure!(path.is_absolute(), "Zed 工作目录必须是绝对路径");
        let path = path.to_str().context("Zed 工作目录不是有效 UTF-8")?;
        let encoded = path
            .split('/')
            .map(urlencoding::encode)
            .collect::<Vec<_>>()
            .join("/");
        Ok(format!("zed://ssh/{}{encoded}", self.ssh_host))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_links_preserve_authority_and_encode_path() {
        for host in ["DevBox", "me@host:2222", "me@[::1]:2222"] {
            let config = Config {
                ssh_host: host.into(),
            };
            assert_eq!(
                config.link(Path::new("/任务/a b#?%\u{1b}")).unwrap(),
                format!("zed://ssh/{host}/%E4%BB%BB%E5%8A%A1/a%20b%23%3F%25%1B")
            );
            assert!(config.link(Path::new("relative")).is_err());
        }
    }

    #[test]
    fn rejects_non_authorities_and_terminal_controls() {
        for host in [
            "",
            "host/path",
            "ssh://host",
            "me:password@host",
            "host:wrong",
            "host:65536",
            "host\n",
            "host\u{1b}",
            "host?x",
            "host#x",
            "host%1b",
            "host\\path",
        ] {
            assert!(
                Config {
                    ssh_host: host.into()
                }
                .validate()
                .is_err(),
                "{host:?}"
            );
        }
    }
}
