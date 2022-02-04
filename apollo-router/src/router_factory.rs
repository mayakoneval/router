use crate::configuration::Configuration;
use crate::reqwest_subgraph_service::ReqwestSubgraphService;
use apollo_router_core::prelude::*;
use apollo_router_core::{
    Context, PluggableRouterServiceBuilder, Plugin, RouterRequest, RouterResponse, Schema,
    SubgraphRequest,
};
use http::{Request, Response};
use static_assertions::assert_impl_all;
use std::sync::Arc;
use tower::buffer::Buffer;
use tower::util::BoxCloneService;
use tower::{BoxError, ServiceBuilder, ServiceExt};
use tower_service::Service;
use tracing::instrument::WithSubscriber;
use typed_builder::TypedBuilder;

/// Factory for creating a RouterService
///
/// Instances of this traits are used by the StateMachine to generate a new
/// RouterService from configuration when it changes
#[async_trait::async_trait]
pub trait RouterServiceFactory: Send + Sync + 'static {
    type RouterService: Service<Request<graphql::Request>, Response = Response<graphql::Response>, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static;

    async fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<graphql::Schema>,
        previous_router: Option<Self::RouterService>,
    ) -> Self::RouterService;
}

assert_impl_all!(ApolloRouterFactory: Send);
/// Main implementation of the RouterService factory, supporting the extensions system
#[derive(Default, TypedBuilder)]
pub struct ApolloRouterFactory {
    plugins: Vec<Box<dyn Plugin>>,
    services: Vec<(
        String,
        Buffer<BoxCloneService<SubgraphRequest, RouterResponse, BoxError>, SubgraphRequest>,
    )>,
}

#[async_trait::async_trait]
impl RouterServiceFactory for ApolloRouterFactory {
    type RouterService = Buffer<
        BoxCloneService<Request<graphql::Request>, Response<graphql::Response>, BoxError>,
        Request<graphql::Request>,
    >;
    async fn create(
        &self,
        configuration: &Configuration,
        schema: Arc<Schema>,
        _previous_router: Option<Self::RouterService>,
    ) -> Self::RouterService {
        let dispatcher = configuration
            .subscriber
            .clone()
            .map(tracing::Dispatch::new)
            .unwrap_or_default();
        let buffer = 20000;
        //TODO Use the plugins, services and config tp build the pipeline.
        let mut builder = PluggableRouterServiceBuilder::new(schema, buffer, dispatcher.clone());

        if self.services.is_empty() {
            for (name, subgraph) in &configuration.subgraphs {
                builder = builder.with_subgraph_service(
                    name,
                    ReqwestSubgraphService::new(name.to_string(), subgraph.routing_url.clone()),
                );
            }
        } else {
            for (name, subgraph) in &self.services {
                builder = builder.with_subgraph_service(name, subgraph.clone());
            }
        }

        let (service, worker) = Buffer::pair(
            ServiceBuilder::new().service(
                builder
                    .build()
                    .await
                    .map_request(|http_request| RouterRequest {
                        http_request,
                        context: Context::new(),
                    })
                    .map_response(|response| response.response)
                    .boxed_clone(),
            ),
            buffer,
        );
        tokio::spawn(worker.with_subscriber(dispatcher));
        service
    }
}
