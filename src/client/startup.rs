use super::*;

/// Runs the thin client and enters the main event loop. `view` chooses the
/// workspace this client starts on and the tag that addresses it.
pub fn run_client(view: ClientViewRequest) -> io::Result<()> {
    run_client_with_mode(None, None, view, "connecting to server")
}

#[cfg(unix)]
pub fn run_terminal_attach(terminal_id: String, takeover: bool) -> io::Result<()> {
    run_client_with_mode(
        Some((terminal_id, takeover)),
        Some(AttachEscapeState::default()),
        ClientViewRequest::default(),
        "attaching to terminal",
    )
}

#[cfg(windows)]
pub fn run_terminal_attach(_terminal_id: String, _takeover: bool) -> io::Result<()> {
    debug_assert!(!crate::platform::capabilities().direct_terminal_attach);
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "direct terminal attach is not supported on Windows yet",
    ))
}
