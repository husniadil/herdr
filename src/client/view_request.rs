//! Launch options that choose what one client-owned shell shows and how a
//! socket caller addresses it: `--workspace <workspace_id>` and
//! `--client-tag <tag>`.
//!
//! Both travel in the endpoint hello. The workspace is where this client starts;
//! it never moves the server's focus or any other client. The tag is a
//! caller-chosen label that `client.list` reports and `client.view.focus` accepts.

/// The per-connection view fields a client-owned shell puts in its hello.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientViewRequest {
    pub workspace_id: Option<String>,
    pub client_tag: Option<String>,
}

pub(crate) const WORKSPACE_FLAG: &str = "--workspace";
pub(crate) const CLIENT_TAG_FLAG: &str = "--client-tag";

/// Removes `--workspace` and `--client-tag` (space or `=` form) from a launch
/// command line and returns the remaining arguments with the request.
///
/// Only the default launch (`herdr [options]`) and the hidden `herdr client`
/// mode carry these flags; subcommands keep their own `--workspace` options, so
/// any other command line is returned untouched.
pub fn extract_launch_view_args(
    args: &[String],
) -> Result<(Vec<String>, ClientViewRequest), String> {
    let launches_client = args
        .get(1)
        .is_none_or(|first| first.starts_with('-') || first == "client");
    if !launches_client {
        return Ok((args.to_vec(), ClientViewRequest::default()));
    }

    let mut request = ClientViewRequest::default();
    let mut cleaned = Vec::with_capacity(args.len());
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if index == 0 || arg == "--" {
            if arg == "--" {
                cleaned.extend_from_slice(&args[index..]);
                break;
            }
            cleaned.push(arg.clone());
            index += 1;
            continue;
        }
        let (flag, inline_value) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value.to_owned())),
            None => (arg.as_str(), None),
        };
        let slot = match flag {
            WORKSPACE_FLAG => &mut request.workspace_id,
            CLIENT_TAG_FLAG => &mut request.client_tag,
            _ => {
                cleaned.push(arg.clone());
                index += 1;
                continue;
            }
        };
        let value = match inline_value {
            Some(value) => {
                index += 1;
                value
            }
            None => {
                let Some(value) = args.get(index + 1) else {
                    return Err(format!("missing value for {flag}"));
                };
                index += 2;
                value.clone()
            }
        };
        if slot.is_some() {
            return Err(format!("{flag} given more than once"));
        }
        if flag == WORKSPACE_FLAG && value.is_empty() {
            return Err(format!("{WORKSPACE_FLAG} needs a workspace id"));
        }
        if flag == CLIENT_TAG_FLAG {
            crate::protocol::endpoint::validate_client_tag(&value)
                .map_err(|error| format!("invalid {CLIENT_TAG_FLAG}: {error}"))?;
        }
        *slot = Some(value);
    }
    Ok((cleaned, request))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn default_launch_extracts_both_flags_in_either_form() {
        let (cleaned, request) = extract_launch_view_args(&args(&[
            "herdr",
            "--workspace",
            "w_2",
            "--client-tag=browser-1",
        ]))
        .unwrap();
        assert_eq!(cleaned, args(&["herdr"]));
        assert_eq!(
            request,
            ClientViewRequest {
                workspace_id: Some("w_2".into()),
                client_tag: Some("browser-1".into()),
            }
        );
    }

    #[test]
    fn launch_without_flags_is_unchanged() {
        let (cleaned, request) = extract_launch_view_args(&args(&["herdr"])).unwrap();
        assert_eq!(cleaned, args(&["herdr"]));
        assert_eq!(request, ClientViewRequest::default());
    }

    #[test]
    fn subcommands_keep_their_own_workspace_option() {
        let input = args(&["herdr", "tab", "create", "--workspace", "w_1"]);
        let (cleaned, request) = extract_launch_view_args(&input).unwrap();
        assert_eq!(cleaned, input);
        assert_eq!(request, ClientViewRequest::default());
    }

    #[test]
    fn hidden_client_mode_accepts_the_flags() {
        let (cleaned, request) =
            extract_launch_view_args(&args(&["herdr", "client", "--workspace=w_3"])).unwrap();
        assert_eq!(cleaned, args(&["herdr", "client"]));
        assert_eq!(request.workspace_id.as_deref(), Some("w_3"));
    }

    #[test]
    fn missing_repeated_or_invalid_values_are_errors() {
        assert!(extract_launch_view_args(&args(&["herdr", "--workspace"])).is_err());
        assert!(extract_launch_view_args(&args(&["herdr", "--workspace="])).is_err());
        assert!(
            extract_launch_view_args(&args(&["herdr", "--client-tag", "a", "--client-tag=b"]))
                .is_err()
        );
        assert!(extract_launch_view_args(&args(&["herdr", "--client-tag="])).is_err());
    }
}
