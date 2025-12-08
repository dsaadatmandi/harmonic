use opentelemetry::{global, propagation::Injector};
use opentelemetry_http::{HeaderExtractor, Request};
use opentelemetry_sdk::{propagation::TraceContextPropagator, trace::Tracer};
use tonic::{Status, body::Body, metadata::{MetadataKey, MetadataMap, MetadataValue}};
use tracing::{Span, debug, info_span, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt};

#[cfg(feature = "trace-jaeger")]
use std::sync::OnceLock;


struct MetadataInjector<'a>(&'a mut MetadataMap);

impl<'a> Injector for MetadataInjector<'a> {
    fn set(&mut self, key: &str, value: String) {
        match MetadataKey::from_bytes(key.as_bytes()) {
            Ok(key) => match MetadataValue::try_from(&value) {
                Ok(value) => {
                    self.0.insert(key, value);
                }

                Err(error) => warn!(value, error = format!("{error:#}"), "parse metadata value"),
            },

            Err(error) => warn!(key, error = format!("{error:#}"), "parse metadata key"),
        }
    }
}


pub fn accept_trace(
    request: Request<Body>,
) -> Request<Body> {
    let parent_context = global::get_text_map_propagator(|prop| {
        prop.extract(&HeaderExtractor(request.headers()))
    });
    let _ = Span::current().set_parent(parent_context);

    request
}

pub fn make_span(request: &Request<Body>) -> Span {
    let headers = request.headers();
    info_span!("incoming request", ?headers)
}

pub fn send_trace<T>(mut request: tonic::Request<T>) -> Result<tonic::Request<T>, Status> {
    global::get_text_map_propagator(|prop| {
        let context = Span::current().context();
        prop.inject_context(&context, &mut MetadataInjector(request.metadata_mut()));
    });

    Ok(request)
}

// Store the provider globally so we can shut it down later
#[cfg(feature = "trace-jaeger")]
static TRACER_PROVIDER: OnceLock<opentelemetry_sdk::trace::SdkTracerProvider> = OnceLock::new();

pub fn init_tracer() -> Tracer {
    #[cfg(feature = "trace-stdout")]
    {
        use opentelemetry::{KeyValue, trace::TracerProvider};
        use opentelemetry_sdk::{Resource, trace::{SdkTracerProvider}};

        let exporter = opentelemetry_stdout::SpanExporter::default();
        let resource = Resource::builder()
        .with_attribute(KeyValue::new("service.name", "harmonic"))
        .build();

        let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter)
        .with_resource(resource)
        .build();

        global::set_tracer_provider(provider.clone());

        provider.tracer("harmonic")
    }

    #[cfg(feature = "trace-jaeger")]
    {
        use opentelemetry::{KeyValue, trace::TracerProvider};
        use opentelemetry_sdk::{Resource, trace::SdkTracerProvider};

        // Using HTTP endpoint (default port 4318 for OTLP/HTTP)
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .build()
            .expect("Failed to create Jaeger OTLP exporter");

        let resource = Resource::builder()
            .with_attribute(KeyValue::new("service.name", "harmonic"))
            .build();

        let provider = SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(resource)
            .build();

        global::set_tracer_provider(provider.clone());

        let tracer = provider.tracer("harmonic");

        // Store provider for later shutdown
        let _ = TRACER_PROVIDER.set(provider);

        tracer
    }

    #[cfg(not(any(feature = "trace-stdout", feature = "trace-jaeger")))]
    {
        use opentelemetry::trace::{TracerProvider, noop::NoopTracerProvider};

        NoopTracerProvider::new().tracer("harmonic")
    }
}

pub fn init_tracing(log_level: &String) {
    let tracer = init_tracer();

    let telem_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stdout);

    let subscriber = tracing_subscriber::registry()
    .with(telem_layer)
    .with(fmt_layer)
    .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level)));

    match tracing::subscriber::set_global_default(subscriber) {
        Ok(_) => (),
        Err(_) => tracing::error!("Error setting global tracing subscriber")
    }
}

pub fn tracing_orchestrator(log_level: &String) {
    global::set_text_map_propagator(TraceContextPropagator::new());

    init_tracing(&log_level);

    debug!("Text map propagator set and tracing initialized");

}

pub async fn shutdown_tracer() {
    use tracing::info;

    info!("Shutting down tracer...");

    #[cfg(feature = "trace-jaeger")]
    {
        if let Some(provider) = TRACER_PROVIDER.get() {
            // Shutdown the tracer provider in a blocking task
            // This flushes all pending spans before shutting down
            if let Err(e) = tokio::task::spawn_blocking(move || {
                if let Err(err) = provider.shutdown() {
                    tracing::error!("Provider shutdown error: {:?}", err);
                }
            }).await {
                tracing::error!("Error in shutdown task: {:?}", e);
            }
            info!("Tracer shutdown complete");
        }
    }

    #[cfg(not(feature = "trace-jaeger"))]
    {
        info!("No tracer to shutdown");
    }
}