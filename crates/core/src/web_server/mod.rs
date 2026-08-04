use actix_web::{App, HttpResponse, HttpServer, Responder, dev::Server, get, post, web};
use sqlx::{Pool, Postgres};
use tokio::sync::mpsc::Sender;

use crate::{configuration::Configuration, kafka_handler::KafkaChannelMessage};

pub fn init_request_handler(
    pool: &Pool<Postgres>,
    app_configuration: &Configuration,
    kafka_thread_sender: &Sender<KafkaChannelMessage>,
) -> Result<Server, Box<dyn std::error::Error>> {
    let pool_cloned = pool.clone();
    let kafka_thread_sender_cloned = kafka_thread_sender.clone();
    Ok(HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(pool_cloned.clone()))
            .app_data(web::Data::new(kafka_thread_sender_cloned.clone()))
            .service(hello)
            .service(echo)
            .route("/hey", web::get().to(manual_hello))
    })
    .disable_signals()
    .bind(("0.0.0.0", app_configuration.port))?
    .run())
}

#[get("/")]
async fn hello(
    kafka_thread_sender: web::Data<tokio::sync::mpsc::Sender<KafkaChannelMessage>>,
) -> impl Responder {
    let kafka_write_result = kafka_thread_sender
        .send(KafkaChannelMessage {
            sender: String::from("base_endpoint"),
            content: String::from("Hello world!"),
            topic: String::from("ninoverse"),
        })
        .await;
    if let Err(kafka_write_error) = kafka_write_result {
        println!("{}", kafka_write_error);
    }
    HttpResponse::Ok().body("Hello world!")
}

#[post("/echo")]
async fn echo(req_body: String) -> impl Responder {
    HttpResponse::Ok().body(req_body)
}

pub async fn manual_hello() -> impl Responder {
    HttpResponse::Ok().body("Hey there!")
}
