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
                // A following option is not this one's value: taken as one,
                // `--workspace --client-tag=t` dropped the tag in silence.
                let Some(value) = args.get(index + 1).filter(|value| !value.starts_with('-'))
                else {
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
    // Stripped from in front of a subcommand, the flags left one that no longer
    // reaches the CLI, and a client was launched in its place.
    let names_a_subcommand = cleaned
        .iter()
        .enumerate()
        .skip(1)
        .take_while(|(_, arg)| arg.as_str() != "--")
        .any(|(index, arg)| !(arg.starts_with('-') || index == 1 && arg == "client"));
    if request != ClientViewRequest::default() && names_a_subcommand {
        return Err(format!(
            "{WORKSPACE_FLAG} and {CLIENT_TAG_FLAG} apply only to launching a client"
        ));
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

    #[test]
    fn a_flag_does_not_take_the_next_flag_as_its_value() {
        assert_eq!(
            extract_launch_view_args(&args(&["herdr", "--workspace", "--client-tag=browser-1"])),
            Err("missing value for --workspace".to_owned())
        );
        assert_eq!(
            extract_launch_view_args(&args(&["herdr", "--client-tag", "--workspace", "w_1"])),
            Err("missing value for --client-tag".to_owned())
        );
        // The `=` form still takes any valid tag.
        let (_, request) =
            extract_launch_view_args(&args(&["herdr", "--client-tag=-dash"])).unwrap();
        assert_eq!(request.client_tag.as_deref(), Some("-dash"));
    }

    #[test]
    fn the_flags_before_a_subcommand_are_refused_rather_than_dropped() {
        // Stripped, they left `herdr workspace list`, which no longer reaches the
        // CLI and launched a client instead.
        for input in [
            &["herdr", "--workspace", "w_1", "workspace", "list"][..],
            &["herdr", "--client-tag=t", "pane", "list"][..],
        ] {
            assert!(extract_launch_view_args(&args(input)).is_err(), "{input:?}");
        }
        // Other launch options keep their own handling.
        let input = args(&["herdr", "--version"]);
        assert_eq!(
            extract_launch_view_args(&input).unwrap(),
            (input, ClientViewRequest::default())
        );
    }
}
