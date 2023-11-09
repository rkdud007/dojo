use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use http::header::CONTENT_TYPE;
use http::{HeaderName, Method, Uri};
use hyper::client::connect::dns::GaiResolver;
use hyper::client::HttpConnector;
use hyper::server::conn::AddrStream;
use hyper::service::make_service_fn;
use hyper::{Body, Request, Response, Server, StatusCode};
use hyper_reverse_proxy::ReverseProxy;
use serde_json::json;
use tokio::sync::RwLock;
use tower::ServiceBuilder;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::error;

const DEFAULT_ALLOW_HEADERS: [&str; 12] = [
    "accept",
    "origin",
    "content-type",
    "access-control-allow-origin",
    "upgrade",
    "x-grpc-web",
    "x-grpc-timeout",
    "x-user-agent",
    "connection",
    "upgrade",
    "sec-websocket-key",
    "sec-websocket-version",
];
const DEFAULT_EXPOSED_HEADERS: [&str; 3] =
    ["grpc-status", "grpc-message", "grpc-status-details-bin"];
const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

lazy_static::lazy_static! {
    static ref  PROXY_CLIENT: ReverseProxy<HttpConnector<GaiResolver>> = {
        ReverseProxy::new(
            hyper::Client::new(),
        )
    };
}

pub struct Proxy {
    addr: SocketAddr,
    allowed_origins: Vec<String>,
    grpc_addr: Option<SocketAddr>,
    graphql_addr: Arc<RwLock<Option<SocketAddr>>>,
}

impl Proxy {
    pub fn new(
        addr: SocketAddr,
        allowed_origins: Vec<String>,
        grpc_addr: Option<SocketAddr>,
        graphql_addr: Option<SocketAddr>,
    ) -> Self {
        Self { addr, allowed_origins, grpc_addr, graphql_addr: Arc::new(RwLock::new(graphql_addr)) }
    }

    pub async fn set_graphql_addr(&self, addr: SocketAddr) {
        let mut graphql_addr = self.graphql_addr.write().await;
        *graphql_addr = Some(addr);
    }

    pub async fn start(
        &self,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> Result<(), hyper::Error> {
        let addr = self.addr;
        let allowed_origins = self.allowed_origins.clone();
        let grpc_addr = self.grpc_addr;
        let graphql_addr = self.graphql_addr.clone();

        let make_svc = make_service_fn(move |conn: &AddrStream| {
            let remote_addr = conn.remote_addr().ip();
            let cors = CorsLayer::new()
                .max_age(DEFAULT_MAX_AGE)
                .allow_methods([Method::GET, Method::POST])
                .allow_headers(
                    DEFAULT_ALLOW_HEADERS
                        .iter()
                        .cloned()
                        .map(HeaderName::from_static)
                        .collect::<Vec<HeaderName>>(),
                )
                .expose_headers(
                    DEFAULT_EXPOSED_HEADERS
                        .iter()
                        .cloned()
                        .map(HeaderName::from_static)
                        .collect::<Vec<HeaderName>>(),
                );

            let cors = match allowed_origins.as_slice() {
                [origin] if origin == "*" => cors.allow_origin(AllowOrigin::mirror_request()),
                origins => cors.allow_origin(
                    origins.iter().map(|o| o.parse().expect("valid origin")).collect::<Vec<_>>(),
                ),
            };

            let graphql_addr_clone = graphql_addr.clone();
            let service = ServiceBuilder::new().layer(cors).service_fn(move |req| {
                let graphql_addr = graphql_addr_clone.clone();
                async move {
                    let graphql_addr = graphql_addr.read().await;
                    handle(remote_addr, grpc_addr, *graphql_addr, req).await
                }
            });

            async { Ok::<_, Infallible>(service) }
        });

        let server = Server::bind(&addr).serve(make_svc);
        server
            .with_graceful_shutdown(async move {
                // Wait for the shutdown signal
                shutdown_rx.recv().await.ok();
            })
            .await
    }
}

async fn handle(
    client_ip: IpAddr,
    grpc_addr: Option<SocketAddr>,
    graphql_addr: Option<SocketAddr>,
    req: Request<Body>,
) -> Result<Response<Body>, Infallible> {
    if req.uri().path().starts_with("/graphql") {
        if let Some(graphql_addr) = graphql_addr {
            let graphql_addr = format!("http://{}", graphql_addr);
            return match PROXY_CLIENT.call(client_ip, &graphql_addr, req).await {
                Ok(response) => Ok(response),
                Err(_error) => {
                    error!("{:?}", _error);
                    Ok(Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Body::empty())
                        .unwrap())
                }
            };
        } else {
            return Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap());
        }
    }

    if req.uri().path().starts_with("/grpc") {
        if let Some(grpc_addr) = grpc_addr {
            let uri = req.uri().clone();
            let (mut parts, body) = req.into_parts();
            parts.uri = Uri::builder()
                .scheme("http")
                .authority("replace.com")
                .path_and_query(uri.path().trim_start_matches("/grpc"))
                .build()
                .unwrap();
            let req = Request::from_parts(parts, body);

            let grpc_addr = format!("http://{}", grpc_addr);
            return match PROXY_CLIENT.call(client_ip, &grpc_addr, req).await {
                Ok(response) => Ok(response),
                Err(_error) => {
                    error!("{:?}", _error);
                    Ok(Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Body::empty())
                        .unwrap())
                }
            };
        } else {
            return Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap());
        }
    }

    let json = json!({
        "service": "torii",
        "success": true
    });
    let body = Body::from(json.to_string());
    let response = Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/json")
        .body(body)
        .unwrap();
    Ok(response)
}