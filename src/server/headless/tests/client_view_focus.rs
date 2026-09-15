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
            snapshot_acks: false,
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
        pane_id: None,
        wait: false,
        timeout_ms: None,
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
async fn client_view_focus_on_a_pane_focuses_it_in_its_tab_for_that_client() {
    let mut server = test_headless_server();
    let first = crate::workspace::Workspace::test_new("first");
    let mut second = crate::workspace::Workspace::test_new("second");
    let left = second.tabs[0].root_pane;
    let right = second.test_split(ratatui::layout::Direction::Horizontal);
    second.tabs[0].layout.focus_pane(right);
    second.test_add_tab(Some("other"));
    server.app.state.workspaces = vec![first, second];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let snapshot = server.app.session_snapshot();
    let second_id = snapshot.workspaces[1].workspace_id.clone();
    let first_id = snapshot.workspaces[0].workspace_id.clone();
    let second_tab = server.app.public_tab_id(1, 0).expect("second tab");
    let first_tab = server.app.public_tab_id(0, 0).expect("first tab");
    let other_tab = server
        .app
        .public_tab_id(1, 1)
        .expect("second workspace's other tab");
    let left_id = server.app.public_pane_id(1, left).expect("left pane id");
    let a = connect_shell_with_view(&mut server, 7, None, Some("a"));
    let b = connect_shell_with_view(&mut server, 8, None, Some("b"));
    let _ = first_snapshot(&a);
    let _ = first_snapshot(&b);

    let response = call_api(
        &mut server,
        api::schema::Method::ClientViewFocus(api::schema::ClientViewFocusParams {
            client_id: None,
            client_tag: Some("b".into()),
            workspace_id: second_id.clone(),
            tab_id: None,
            pane_id: Some(left_id.clone()),
            wait: false,
            timeout_ms: None,
        }),
    );

    assert_eq!(response["result"]["type"], "client_view_focus");
    assert_eq!(response["result"]["client"]["tab_id"], second_tab.as_str());
    assert_eq!(response["result"]["client"]["pane_id"], left_id.as_str());
    assert_eq!(response["result"]["client"]["zoomed"], false);
    assert_eq!(
        server.app.state.workspaces[1].tabs[0].layout.focused(),
        left
    );
    assert_eq!(server.app.state.active, Some(0), "the server's focus stays");
    // A pane outside the workspace, or outside the tab named with it, is refused.
    for (workspace, tab) in [(&first_id, None), (&second_id, Some(&other_tab))] {
        let refused = call_api(
            &mut server,
            api::schema::Method::ClientViewFocus(api::schema::ClientViewFocusParams {
                client_id: None,
                client_tag: Some("b".into()),
                workspace_id: workspace.clone(),
                tab_id: tab.cloned(),
                pane_id: Some(left_id.clone()),
                wait: false,
                timeout_ms: None,
            }),
        );
        assert_eq!(refused["error"]["code"], "pane_not_found");
    }
    assert_eq!(
        server.shell_tab_id_for_client(7).as_deref(),
        Some(first_tab.as_str())
    );
    server.render_and_stream();
    assert!(
        next_snapshot(&a).is_none(),
        "a client on another workspace is not sent a new view"
    );
    let moved = next_snapshot(&b).expect("the addressed client's new view");
    assert_eq!(moved.focused_pane_id.as_deref(), Some(left_id.as_str()));

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
    let clients = &response["result"]["clients"];
    assert_eq!(clients.as_array().map(Vec::len), Some(2));
    for (client, id, tag, workspace, tab) in [
        (&clients[0], 7, None, &workspace_ids[0], &tab_ids[0]),
        (&clients[1], 8, Some("b"), &workspace_ids[1], &tab_ids[1]),
    ] {
        assert_eq!(client["client_id"], id);
        assert_eq!(client["client_tag"].as_str(), tag);
        assert_eq!(client["workspace_id"], workspace.as_str());
        assert_eq!(client["tab_id"], tab.as_str());
    }
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

fn connect_acking_shell(
    server: &mut HeadlessServer,
    client_id: u64,
    client_tag: &str,
) -> (
    std::sync::mpsc::Receiver<Vec<u8>>,
    std::sync::mpsc::Receiver<Vec<u8>>,
) {
    let (writer, control, render) = test_client_writer();
    assert!(
        server.handle_server_event(ServerEvent::ClientShellConnected {
            surface_reuse: false,
            surface_delta: false,
            workspace_id: None,
            client_tag: Some(client_tag.to_owned()),
            snapshot_acks: true,
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
    // The render channel stays open so a render does not drop the client.
    (control, render)
}

fn acknowledge(server: &mut HeadlessServer, client_id: u64, revision: u64) {
    let boot_id = server.client_shell_boot_id.clone();
    assert!(
        !server.handle_server_event(ServerEvent::ClientShellSnapshotApplied {
            client_id,
            boot_id,
            revision,
        }),
        "an acknowledgement alone never renders"
    );
}

/// Sends a request and returns the channel its answer arrives on, which may be later.
fn start_api(
    server: &mut HeadlessServer,
    method: api::schema::Method,
) -> std::sync::mpsc::Receiver<String> {
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    server.handle_api_request_with_shutdown_check(crate::api::ApiRequestMessage {
        request: api::schema::Request {
            id: "test.client.view.wait".into(),
            method,
        },
        respond_to,
        response_write_complete: None,
        stream_active: None,
    });
    response_rx
}

fn answered(response_rx: &std::sync::mpsc::Receiver<String>) -> Option<serde_json::Value> {
    response_rx
        .try_recv()
        .ok()
        .map(|response| serde_json::from_str(&response).expect("json response"))
}

fn waiting_focus(
    client_tag: &str,
    workspace_id: &str,
    tab_id: Option<&str>,
    timeout_ms: Option<u64>,
) -> api::schema::Method {
    api::schema::Method::ClientViewFocus(api::schema::ClientViewFocusParams {
        client_id: None,
        client_tag: Some(client_tag.to_owned()),
        workspace_id: workspace_id.to_owned(),
        tab_id: tab_id.map(str::to_owned),
        pane_id: None,
        wait: true,
        timeout_ms,
    })
}

fn view_wait(client_tag: &str, timeout_ms: Option<u64>) -> api::schema::Method {
    api::schema::Method::ClientViewWait(api::schema::ClientViewWaitParams {
        client_id: None,
        client_tag: Some(client_tag.to_owned()),
        timeout_ms,
    })
}

fn client_list(server: &mut HeadlessServer) -> serde_json::Value {
    call_api(
        server,
        api::schema::Method::ClientList(api::schema::EmptyParams::default()),
    )["result"]["clients"]
        .clone()
}

#[tokio::test]
async fn client_list_reports_revisions_panes_and_applied_views() {
    let (mut server, workspace_ids, tab_ids) = two_workspace_server();
    let _old = connect_shell_with_view(&mut server, 7, None, None);
    let _acking = connect_acking_shell(&mut server, 8, "b");
    let first_pane = server
        .app
        .public_pane_id(0, server.app.state.workspaces[0].tabs[0].layout.focused());

    assert_eq!(
        client_list(&mut server),
        serde_json::json!([
            {
                "client_id": 7,
                "workspace_id": workspace_ids[0],
                "tab_id": tab_ids[0],
                "pane_id": first_pane,
                "zoomed": false,
                "snapshot_acks": false,
                "revision": 1,
                "view_revision": 1,
                "view_applied": false,
            },
            {
                "client_id": 8,
                "client_tag": "b",
                "workspace_id": workspace_ids[0],
                "tab_id": tab_ids[0],
                "pane_id": first_pane,
                "zoomed": false,
                "snapshot_acks": true,
                "revision": 1,
                "view_revision": 1,
                "view_applied": false,
            },
        ])
    );

    let other_boot = ServerEvent::ClientShellSnapshotApplied {
        client_id: 8,
        boot_id: "another-boot".into(),
        revision: 1,
    };
    assert!(!server.handle_server_event(other_boot));
    acknowledge(&mut server, 8, 2);
    assert_eq!(
        client_list(&mut server)[1]["applied_revision"],
        serde_json::Value::Null,
        "an acknowledgement for another boot or an unsent revision is ignored"
    );

    acknowledge(&mut server, 8, 1);
    acknowledge(&mut server, 7, 1);
    let clients = client_list(&mut server);
    assert_eq!(clients[1]["applied_revision"], 1);
    assert_eq!(clients[1]["view_applied"], true);
    assert_eq!(
        clients[0]["view_applied"], false,
        "a client whose hello did not offer acknowledgements is never reported as applied"
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn waiting_view_focus_answers_only_after_the_client_applies_the_new_view() {
    let (mut server, workspace_ids, tab_ids) = two_workspace_server();
    let (control, _render) = connect_acking_shell(&mut server, 8, "b");
    let seed = first_snapshot(&control);
    acknowledge(&mut server, 8, seed.revision);

    let response_rx = start_api(
        &mut server,
        waiting_focus("b", &workspace_ids[1], Some(&tab_ids[2]), None),
    );
    assert_eq!(
        server.shell_tab_id_for_client(8).as_deref(),
        Some(tab_ids[2].as_str()),
        "the view moves before the answer"
    );
    server.poll_pending_client_view_waits(Instant::now());
    assert!(answered(&response_rx).is_none(), "no snapshot was sent yet");

    server.render_and_stream();
    let moved = next_snapshot(&control).expect("snapshot showing the new view");
    assert_eq!(moved.focused_tab_id.as_deref(), Some(tab_ids[2].as_str()));
    server.poll_pending_client_view_waits(Instant::now());
    assert!(
        answered(&response_rx).is_none(),
        "a sent snapshot is not an applied one"
    );

    acknowledge(&mut server, 8, seed.revision);
    server.poll_pending_client_view_waits(Instant::now());
    assert!(
        answered(&response_rx).is_none(),
        "the acknowledged snapshot still shows the old view"
    );

    acknowledge(&mut server, 8, moved.revision);
    server.poll_pending_client_view_waits(Instant::now());
    let response = answered(&response_rx).expect("answer once the new view is applied");
    assert_eq!(response["result"]["type"], "client_view_focus");
    let client = &response["result"]["client"];
    assert_eq!(client["tab_id"], tab_ids[2].as_str());
    assert_eq!(
        client["pane_id"],
        moved.focused_pane_id.clone().unwrap().as_str()
    );
    assert_eq!(client["applied_revision"], moved.revision);
    assert_eq!(client["view_applied"], true);
    assert!(server.pending_client_view_waits.is_empty());
    assert_eq!(server.app.state.active, Some(0));
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn view_wait_times_out_when_the_client_never_acknowledges() {
    let (mut server, workspace_ids, tab_ids) = two_workspace_server();
    let (control, _render) = connect_acking_shell(&mut server, 8, "b");
    let _ = first_snapshot(&control);

    let response_rx = start_api(
        &mut server,
        waiting_focus("b", &workspace_ids[1], None, Some(250)),
    );
    server.render_and_stream();
    server.poll_pending_client_view_waits(Instant::now());
    assert!(answered(&response_rx).is_none());
    let deadline = server.pending_client_view_waits[0].deadline;

    server.poll_pending_client_view_waits(deadline);
    let response = answered(&response_rx).expect("timeout answer at the deadline");
    assert_eq!(response["error"]["code"], "timeout", "{response}");
    assert!(server.pending_client_view_waits.is_empty());
    assert_eq!(
        server.shell_tab_id_for_client(8).as_deref(),
        Some(tab_ids[1].as_str()),
        "a timed-out wait leaves the view where it was moved"
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn view_wait_follows_a_zoom_of_the_viewed_tab() {
    let (mut server, _, _) = two_workspace_server();
    let (control, _render) = connect_acking_shell(&mut server, 8, "b");
    let seed = first_snapshot(&control);
    acknowledge(&mut server, 8, seed.revision);

    let applied = call_api(&mut server, view_wait("b", None));
    assert_eq!(applied["result"]["type"], "client_view_wait");
    assert_eq!(applied["result"]["client"]["zoomed"], false);

    // What `pane.zoom` changes; the client's routing snapshot now differs from it.
    server.app.state.workspaces[0].tabs[0].zoomed = true;
    let response_rx = start_api(&mut server, view_wait("b", None));
    server.poll_pending_client_view_waits(Instant::now());
    assert!(answered(&response_rx).is_none());

    server.render_and_stream();
    let zoomed = next_snapshot(&control).expect("snapshot carrying the zoom");
    acknowledge(&mut server, 8, zoomed.revision);
    server.poll_pending_client_view_waits(Instant::now());
    let response = answered(&response_rx).expect("answer once the zoom is applied");
    assert_eq!(response["result"]["client"]["zoomed"], true, "{response}");
    assert_eq!(
        response["result"]["client"]["view_revision"],
        zoomed.revision
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn view_waits_refuse_clients_without_acknowledgements_before_moving_them() {
    let (mut server, workspace_ids, tab_ids) = two_workspace_server();
    let _old = connect_shell_with_view(&mut server, 7, None, Some("old"));
    let _acking = connect_acking_shell(&mut server, 8, "b");

    let focus = call_api(
        &mut server,
        waiting_focus("old", &workspace_ids[1], None, None),
    );
    assert_eq!(
        focus["error"]["code"], "client_view_ack_unsupported",
        "{focus}"
    );
    assert_eq!(
        server.shell_tab_id_for_client(7).as_deref(),
        Some(tab_ids[0].as_str())
    );
    let wait = call_api(&mut server, view_wait("old", None));
    assert_eq!(
        wait["error"]["code"], "client_view_ack_unsupported",
        "{wait}"
    );

    let too_long = call_api(
        &mut server,
        waiting_focus("b", &workspace_ids[1], None, Some(60_001)),
    );
    assert_eq!(too_long["error"]["code"], "invalid_params", "{too_long}");
    assert_eq!(
        server.shell_tab_id_for_client(8).as_deref(),
        Some(tab_ids[0].as_str())
    );
    let missing = call_api(&mut server, view_wait("missing", None));
    assert_eq!(missing["error"]["code"], "client_not_found", "{missing}");

    let plain = call_api(
        &mut server,
        focus_params(None, Some("old"), &workspace_ids[1], None),
    );
    assert_eq!(plain["result"]["type"], "client_view_focus");
    assert!(server.pending_client_view_waits.is_empty());
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn view_wait_ends_when_the_client_disconnects() {
    let (mut server, _, _) = two_workspace_server();
    let (control, _render) = connect_acking_shell(&mut server, 8, "b");
    let _ = first_snapshot(&control);

    let response_rx = start_api(&mut server, view_wait("b", None));
    server.poll_pending_client_view_waits(Instant::now());
    assert!(answered(&response_rx).is_none());

    assert!(server.handle_server_event(ServerEvent::ClientDisconnected { client_id: 8 }));
    server.poll_pending_client_view_waits(Instant::now());
    let response = answered(&response_rx).expect("answer when the client goes");
    assert_eq!(response["error"]["code"], "client_not_found", "{response}");
    shutdown_test_runtimes(&mut server);
}
