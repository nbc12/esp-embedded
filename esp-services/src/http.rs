use embassy_net::Stack;

/// Runs a picoserve server.
pub async fn run_http<App: picoserve::AppBuilder>(
    task_id: usize,
    stack: Stack<'static>,
    app_builder: App,
    config: &'static picoserve::Config,
) -> ! 
{
    let mut tcp_rx = [0u8; 1536];
    let mut tcp_tx = [0u8; 1536];
    let mut http_buf = [0u8; 2048];

    let app = app_builder.build_app();

    picoserve::Server::new(&app, config, &mut http_buf)
        .listen_and_serve(task_id, stack, 80, &mut tcp_rx, &mut tcp_tx)
        .await
        .into_never()
}
