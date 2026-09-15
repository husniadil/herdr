use super::*;

fn connect_shell_with_view(
    server: &mut HeadlessServer,
    client_id: u64,
    workspace_id: Option<&str>,
    client_tag: Option<&str>,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (writer, control, _render) = test_client_writer();
    assert!(
        server.handle_server_event(ServerEvent::ClientShellConnected {
            workspace_id: workspace_id.map(str::to_owned),
            client_tag: client_tag.map(str::to_owned),
            surface_reuse: false,
            surface_delta: false,
            client_id,
            surface_cols: 80,
            surface_rows: 23,
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_mouse: false,
            direct_graphics: false,
            endpoint_keybindings: false,
            mouse_capture: false,
            surface_active: true,
            writer,
        })
    );
    control
}

/// Two workspaces with the server focused on the first; the second has two tabs.
fn two_workspace_server() -> (HeadlessServer, [String; 2], [String; 3]) {
    let mut server = test_headless_server();
    let first = crate::workspace::Workspace::test_new("first");
    let mut second = crate::workspace::Workspace::test_new("second");
    second.test_add_tab(Some("second-b"));
    server.app.state.workspaces = vec![first, second];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let snapshot = server.app.session_snapshot();
    let workspace_ids = [
        snapshot.workspaces[0].workspace_id.clone(),
        snapshot.workspaces[1].workspace_id.clone(),
    ];
    let tab_ids = [
        server.app.public_tab_id(0, 0).expect("first tab"),
        server.app.public_tab_id(1, 0).expect("second tab"),
        server
            .app
            .public_tab_id(1, 1)
            .expect("second workspace second tab"),
    ];
    (server, workspace_ids, tab_ids)
}

fn call_api(server: &mut HeadlessServer, method: api::schema::Method) -> serde_json::Value {
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    server.handle_api_request_with_shutdown_check(crate::api::ApiRequestMessage {
        request: api::schema::Request {
            id: "test.client.view".into(),
            method,
        },
        respond_to,
        response_write_complete: None,
        stream_active: None,
    });
    serde_json::from_str(&response_rx.recv().expect("api response")).expect("json response")
}

fn focus_params(
    client_id: Option<u64>,
    client_tag: Option<&str>,
    workspace_id: &str,
    tab_id: Option<&str>,
) -> api::schema::Method {
    api::schema::Method::ClientViewFocus(api::schema::ClientViewFocusParams {
        client_id,
        client_tag: client_tag.map(str::to_owned),
        workspace_id: workspace_id.to_owned(),
        tab_id: tab_id.map(str::to_owned),
    })
}

fn first_snapshot(
    control: &std::sync::mpsc::Receiver<Vec<u8>>,
) -> Box<crate::protocol::ClientShellSnapshot> {
    client_shell_snapshot(read_server_message(
        control.recv().expect("initial shell snapshot"),
    ))
}

/// Next shell snapshot on a control channel, skipping other control messages.
fn next_snapshot(
    control: &std::sync::mpsc::Receiver<Vec<u8>>,
) -> Option<Box<crate::protocol::ClientShellSnapshot>> {
    while let Ok(bytes) = control.recv_timeout(Duration::from_millis(200)) {
        let message = read_server_message(bytes);
        if matches!(&message, ServerMessage::EndpointControl { kind, .. } if kind == crate::protocol::endpoint::ENDPOINT_SNAPSHOT_KIND)
        {
            return Some(client_shell_snapshot(message));
        }
    }
    None
}

#[tokio::test]
async fn client_started_on_a_workspace_shows_it_without_moving_server_focus() {
    let (mut server, workspace_ids, tab_ids) = two_workspace_server();

    let existing = connect_shell_with_view(&mut server, 7, None, None);
    let started = connect_shell_with_view(&mut server, 8, Some(&workspace_ids[1]), Some("b"));

    let existing_snapshot = first_snapshot(&existing);
    let started_snapshot = first_snapshot(&started);
    assert_eq!(
        existing_snapshot.focused_workspace_id.as_deref(),
        Some(workspace_ids[0].as_str())
    );
    assert_eq!(
        started_snapshot.focused_workspace_id.as_deref(),
        Some(workspace_ids[1].as_str())
    );
    assert_eq!(
        started_snapshot.focused_tab_id.as_deref(),
        Some(tab_ids[1].as_str())
    );
    assert_eq!(server.app.state.active, Some(0));
    assert_eq!(
        server.shell_tab_id_for_client(7).as_deref(),
        Some(tab_ids[0].as_str())
    );
    assert_eq!(
        server.shell_tab_id_for_client(8).as_deref(),
        Some(tab_ids[1].as_str())
    );
    assert_eq!(server.clients[&8].client_tag.as_deref(), Some("b"));

    server.render_and_stream();
    assert!(
        next_snapshot(&existing).is_none(),
        "starting another client on a workspace must not replace this client's view"
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn unknown_start_workspace_falls_back_to_server_focus() {
    let (mut server, workspace_ids, _) = two_workspace_server();

    for (client_id, requested) in [(7, "w_missing"), (8, "99")] {
        let control = connect_shell_with_view(&mut server, client_id, Some(requested), None);
        assert_eq!(
            first_snapshot(&control).focused_workspace_id.as_deref(),
            Some(workspace_ids[0].as_str())
        );
    }
    assert_eq!(server.app.state.active, Some(0));
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn hello_without_view_fields_starts_on_server_focus() {
    let (mut server, workspace_ids, tab_ids) = two_workspace_server();
    server.app.state.active = Some(1);
    server.app.state.selected = 1;

    let control = connect_shell_with_view(&mut server, 7, None, None);
    let snapshot = first_snapshot(&control);

    assert_eq!(
        snapshot.focused_workspace_id.as_deref(),
        Some(workspace_ids[1].as_str())
    );
    assert_eq!(
        snapshot.focused_tab_id.as_deref(),
        Some(tab_ids[1].as_str())
    );
    assert_eq!(server.clients[&7].client_tag, None);
    assert_eq!(server.app.state.active, Some(1));
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_view_focus_moves_only_the_addressed_client() {
    let (mut server, workspace_ids, tab_ids) = two_workspace_server();
    let first = connect_shell_with_view(&mut server, 7, None, Some("a"));
    let second = connect_shell_with_view(&mut server, 8, None, Some("b"));
    let _ = first_snapshot(&first);
    let _ = first_snapshot(&second);

    let response = call_api(
        &mut server,
        focus_params(None, Some("b"), &workspace_ids[1], Some(&tab_ids[2])),
    );

    assert_eq!(response["result"]["type"], "client_view_focus");
    assert_eq!(response["result"]["client"]["client_id"], 8);
    assert_eq!(response["result"]["client"]["client_tag"], "b");
    assert_eq!(
        response["result"]["client"]["workspace_id"],
        workspace_ids[1].as_str()
    );
    assert_eq!(response["result"]["client"]["tab_id"], tab_ids[2].as_str());
    assert_eq!(server.app.state.active, Some(0));
    assert_eq!(server.app.state.workspaces[1].active_tab_index(), 0);
    assert_eq!(
        server.shell_tab_id_for_client(7).as_deref(),
        Some(tab_ids[0].as_str())
    );
    assert_eq!(
        server.shell_tab_id_for_client(8).as_deref(),
        Some(tab_ids[2].as_str())
    );

    server.render_and_stream();
    assert!(
        next_snapshot(&first).is_none(),
        "another client must not receive a view replacement"
    );
    let replacement = next_snapshot(&second).expect("addressed client replacement snapshot");
    assert_eq!(
        replacement.focused_workspace_id.as_deref(),
        Some(workspace_ids[1].as_str())
    );
    assert_eq!(
        replacement.focused_tab_id.as_deref(),
        Some(tab_ids[2].as_str())
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_view_focus_by_id_keeps_the_clients_remembered_tab() {
    let (mut server, workspace_ids, tab_ids) = two_workspace_server();
    let control = connect_shell_with_view(&mut server, 7, None, None);
    let _ = first_snapshot(&control);

    let moved = call_api(
        &mut server,
        focus_params(Some(7), None, &workspace_ids[1], Some(&tab_ids[2])),
    );
    assert_eq!(moved["result"]["client"]["tab_id"], tab_ids[2].as_str());
    let back = call_api(
        &mut server,
        focus_params(Some(7), None, &workspace_ids[0], None),
    );
    assert_eq!(back["result"]["client"]["tab_id"], tab_ids[0].as_str());
    let again = call_api(
        &mut server,
        focus_params(Some(7), None, &workspace_ids[1], None),
    );

    assert_eq!(again["result"]["client"]["tab_id"], tab_ids[2].as_str());
    assert_eq!(server.app.state.active, Some(0));
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_view_focus_sends_the_moved_client_its_own_window_title() {
    let (mut server, workspace_ids, _) = two_workspace_server();
    server.app.configure_window_title("{workspace}");
    let background = connect_shell_with_view(&mut server, 7, None, Some("a"));
    let foreground = connect_shell_with_view(&mut server, 8, None, Some("b"));
    drain_window_titles(&background);
    drain_window_titles(&foreground);

    call_api(
        &mut server,
        focus_params(None, Some("a"), &workspace_ids[1], None),
    );
    assert_eq!(next_window_title(&background), Some(Some("second".into())));
    assert!(no_window_title(&foreground));

    call_api(
        &mut server,
        focus_params(None, Some("b"), &workspace_ids[1], None),
    );
    assert_eq!(next_window_title(&foreground), Some(Some("second".into())));
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_list_reports_tags_and_views() {
    let (mut server, workspace_ids, tab_ids) = two_workspace_server();
    let _untagged = connect_shell_with_view(&mut server, 7, None, None);
    let _tagged = connect_shell_with_view(&mut server, 8, Some(&workspace_ids[1]), Some("b"));

    let response = call_api(
        &mut server,
        api::schema::Method::ClientList(api::schema::EmptyParams::default()),
    );

    assert_eq!(response["result"]["type"], "client_list");
    assert_eq!(
        response["result"]["clients"],
        serde_json::json!([
            {
                "client_id": 7,
                "workspace_id": workspace_ids[0],
                "tab_id": tab_ids[0],
            },
            {
                "client_id": 8,
                "client_tag": "b",
                "workspace_id": workspace_ids[1],
                "tab_id": tab_ids[1],
            },
        ])
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_view_focus_rejects_bad_addresses_and_targets() {
    let (mut server, workspace_ids, tab_ids) = two_workspace_server();
    let _a = connect_shell_with_view(&mut server, 7, None, Some("same"));
    let _b = connect_shell_with_view(&mut server, 8, None, Some("same"));
    let _c = connect_shell_with_view(&mut server, 9, None, Some("solo"));

    let cases = [
        (
            focus_params(None, Some("missing"), &workspace_ids[1], None),
            "client_not_found",
        ),
        (
            focus_params(Some(404), None, &workspace_ids[1], None),
            "client_not_found",
        ),
        (
            focus_params(None, Some("same"), &workspace_ids[1], None),
            "ambiguous_client",
        ),
        (
            focus_params(None, None, &workspace_ids[1], None),
            "invalid_params",
        ),
        (
            focus_params(Some(9), Some("solo"), &workspace_ids[1], None),
            "invalid_params",
        ),
        (
            focus_params(None, Some("solo"), "w_missing", None),
            "workspace_not_found",
        ),
        (
            focus_params(None, Some("solo"), "99", None),
            "workspace_not_found",
        ),
        (
            focus_params(None, Some("solo"), &workspace_ids[1], Some(&tab_ids[0])),
            "tab_not_found",
        ),
    ];
    for (method, code) in cases {
        let response = call_api(&mut server, method);
        assert_eq!(response["error"]["code"], code, "{response}");
    }
    assert_eq!(server.app.state.active, Some(0));
    for client_id in [7, 8, 9] {
        assert_eq!(
            server.shell_tab_id_for_client(client_id).as_deref(),
            Some(tab_ids[0].as_str())
        );
    }
    shutdown_test_runtimes(&mut server);
}
