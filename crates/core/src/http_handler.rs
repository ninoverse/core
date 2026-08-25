pub mod error;

use std::io::prelude::*;

use error::NinoverseHttpHandlerError;
use http::header::{HeaderName, HeaderValue};
use http::method::Method;
use http::request::Builder;
use http::uri::Uri;
use http::version::Version;
use http::{HeaderMap, Request, Response};
use std::str;

#[derive(thiserror::Error, Debug)]
pub enum NinoverseHttpHandlerError {
    #[error("TCP_LISTENER: Error writing in buffer.")]
    BufferError { additional_info: String },
    #[error("TCP_LISTENER: Error handling the TcpStream.")]
    StreamError { additional_info: String },
    #[error("TCP_LISTENER: Error parsing a response.")]
    ParsingError { additional_info: String },
    #[error("TCP_LISTENER: Error building the request struct.")]
    RequestStructError { additional_info: String },
    #[allow(dead_code)]
    #[error("TCP_LISTENER: Error building the response struct.")]
    ResponseStructError { additional_info: String },
}

pub fn response_to_string<T>(response: Response<T>) -> Result<String, NinoverseHttpHandlerError>
where
    T: serde::Serialize,
{
    let response = response;
    let mut buffer = Vec::new();
    write!(buffer, "HTTP/1.1 {}\r\n", response.status().as_str()).or_else(|_| {
        Err(NinoverseHttpHandlerError::BufferError {
            additional_info: String::from("Writing status to buffer."),
        })
    })?;
    for (name, value) in response.headers() {
        let header_value = value.to_str().or_else(|_| {
            Err(NinoverseHttpHandlerError::BufferError {
                additional_info: String::from("Converting header value to &str."),
            })
        })?;
        write!(buffer, "{}: {}\r\n", name, header_value).or_else(|_| {
            Err(NinoverseHttpHandlerError::BufferError {
                additional_info: String::from("Writing header name and value."),
            })
        })?;
    }
    write!(buffer, "\r\n").or_else(|_| {
        Err(NinoverseHttpHandlerError::BufferError {
            additional_info: String::from("Writing basic characters to buffer."),
        })
    })?;
    let body = serde_json::to_string(response.body()).or_else(|_| {
        Err(NinoverseHttpHandlerError::BufferError {
            additional_info: String::from("Serializing body with serde_json."),
        })
    })?;
    write!(buffer, "{}", body).or_else(|_| {
        Err(NinoverseHttpHandlerError::BufferError {
            additional_info: String::from("Writing body to buffer."),
        })
    })?;
    Ok(String::from_utf8(buffer).or_else(|_| {
        Err(NinoverseHttpHandlerError::BufferError {
            additional_info: String::from("Converting buffer to String."),
        })
    })?)
}

pub fn write_to_stream<T>(
    stream: &mut std::net::TcpStream,
    response: Response<T>,
) -> Result<(), NinoverseHttpHandlerError>
where
    T: serde::Serialize,
{
    stream
        .write(response_to_string(response)?.as_bytes())
        .or_else(|_| {
            Err(NinoverseHttpHandlerError::StreamError {
                additional_info: String::from("Writing to stream."),
            })
        })?;
    stream.flush().or_else(|_| {
        Err(NinoverseHttpHandlerError::StreamError {
            additional_info: String::from("Flushing stream."),
        })
    })?;
    Ok(())
}

pub fn read_from_stream<T>(
    stream: &mut std::net::TcpStream,
) -> Result<Request<T>, NinoverseHttpHandlerError>
where
    T: serde::de::DeserializeOwned + Default,
{
    let mut buffer = [0; 1024];
    stream.read(&mut buffer).or_else(|_| {
        Err(NinoverseHttpHandlerError::StreamError {
            additional_info: String::from("Reading stream."),
        })
    })?;
    Ok(parse_request(&buffer)?)
}

fn parse_request_line(
    request_line: &str,
) -> Result<(Method, Uri, Version), NinoverseHttpHandlerError> {
    let mut request_line_parts = request_line.split_whitespace();
    let method = request_line_parts
        .next()
        .unwrap_or_default()
        .parse::<Method>()
        .or_else(|_| {
            Err(NinoverseHttpHandlerError::ParsingError {
                additional_info: String::from("Parsing response method."),
            })
        })?;
    let uri = request_line_parts
        .next()
        .unwrap_or_default()
        .parse::<Uri>()
        .or_else(|_| {
            Err(NinoverseHttpHandlerError::ParsingError {
                additional_info: String::from("Parsing URI."),
            })
        })?;
    let version = match request_line_parts.next().unwrap_or_default() {
        "HTTP/1.1" => Ok(Version::HTTP_11),
        "HTTP/1.0" => Ok(Version::HTTP_10),
        _ => Err(NinoverseHttpHandlerError::StreamError {
            additional_info: String::from("Reading stream."),
        }),
    }?;
    Ok((method, uri, version))
}

fn parse_headers(
    headers: &mut HeaderMap,
    lines: &mut str::Split<&str>,
) -> Result<(), NinoverseHttpHandlerError> {
    for line in lines {
        if line.is_empty() {
            break;
        }
        let mut header_parts = line.splitn(2, ": ");
        let name = header_parts
            .next()
            .unwrap_or_default()
            .parse::<HeaderName>()
            .or_else(|_| {
                Err(NinoverseHttpHandlerError::ParsingError {
                    additional_info: String::from("Parsing header name."),
                })
            })?;
        let value = header_parts
            .next()
            .unwrap_or_default()
            .parse::<HeaderValue>()
            .or_else(|_| {
                Err(NinoverseHttpHandlerError::ParsingError {
                    additional_info: String::from("Parsing header value."),
                })
            })?;
        headers.append(name, value);
    }
    Ok(())
}

fn parse_body<T>(lines: &mut str::Split<&str>) -> Result<T, NinoverseHttpHandlerError>
where
    T: serde::de::DeserializeOwned + Default,
{
    Ok(serde_json::from_str(
        lines
            .next()
            .unwrap_or_default()
            .trim_matches(|c: char| c.is_whitespace() || c.is_control()),
    )
    .or_else(|_| {
        Err(NinoverseHttpHandlerError::ParsingError {
            additional_info: String::from("Parsing body value."),
        })
    })?)
}

fn parse_request<T>(buffer: &[u8]) -> Result<Request<T>, NinoverseHttpHandlerError>
where
    T: serde::de::DeserializeOwned + Default,
{
    let request_str = str::from_utf8(buffer).or_else(|_| {
        Err(NinoverseHttpHandlerError::ParsingError {
            additional_info: String::from("Parsing URI."),
        })
    })?;
    let mut lines = request_str.split("\r\n");

    // Parse the request line
    let request_line = parse_request_line(lines.next().unwrap_or_default())?;
    let method = request_line.0;
    let uri = request_line.1;
    let version = request_line.2;

    // Parse headers
    let mut headers = http::HeaderMap::new();
    parse_headers(&mut headers, &mut lines)?;

    // Parse the body
    let body: T = if method == Method::POST && lines.clone().count() > 0 {
        parse_body(&mut lines)?
    } else {
        T::default()
    };

    // Build the request struct
    let mut request = Builder::new()
        .method(method)
        .uri(uri)
        .version(version)
        .body(body)
        .or_else(|_| {
            Err(NinoverseHttpHandlerError::RequestStructError {
                additional_info: String::from("Building request struct."),
            })
        })?;
    request.headers_mut().extend(headers);
    Ok(request)
}

pub fn extract_uri_pieces_vector<T>(request: &Request<T>) -> Vec<String> {
    let uri_pieces = request
        .uri()
        .path()
        .trim_start_matches('/')
        .split('/')
        .map(|uri_part| String::from(uri_part))
        .collect::<Vec<String>>();
    uri_pieces
}

// EXPERIMENTS

// use std::{net::TcpStream, pin::Pin, sync::Arc};

// use http::Request;
// use sqlx::{Pool, Postgres};

// use super::error::NinoverseApiError;

// pub struct UriSection<'a, T>
// where
//     T: Sync,
// {
//     pub execute_uri_section: Box<
//         dyn Fn(
//                 &'a Request<T>,
//                 &'a TcpStream,
//                 Arc<Pool<Postgres>>,
//             ) -> Pin<Box<dyn Future<Output = Result<(), NinoverseApiError>> + Send>>
//             + Send
//             + Sync
//             +'a,
//     >,
//     pub next_section: Option<Box<UriSection<'a, T>>>,
// }

// pub trait UriSectionFn<'a, T>
// where
//     T: Sync,
// {
//     async fn next(
//         &self,
//         request: &'a Request<T>,
//         stream: &'a TcpStream,
//         pool: Arc<Pool<Postgres>>,
//     ) -> Result<(), NinoverseApiError>;
// }

// impl<'a, T> UriSectionFn<'a, T> for UriSection<'a, T>
// where
//     T: Sync,
// {
//     async fn next(
//         &self,
//         request: &'a Request<T>,
//         stream: &'a TcpStream,
//         pool: Arc<Pool<Postgres>>,
//     ) -> Result<(), NinoverseApiError> {
//         let pool_clone = pool.clone();
//         (self.execute_uri_section)(request, stream, pool).await?;
//         if let Some(next_section) = &self.next_section {
//             Box::pin(next_section.next(request, stream, pool_clone)).await?;
//             Ok(())
//         } else {
//             Ok(())
//         }
//     }
// }

// enum UriSectionType {
//     Root,
//     Branch,
//     Leaf,
// }

// use std::{net::TcpStream, pin::Pin, sync::Arc};

// use http::Request;
// use sqlx::{Executor, Pool, Postgres};

// use super::{UriSection, error::NinoverseApiError};

// async fn test<T>(
//     request: &Request<T>,
//     _stream: &TcpStream,
//     _pool: &Arc<Pool<Postgres>>,
// ) -> Result<(), NinoverseApiError>
// where
//     T: Sync,
// {
//     println!("Executing URI section logic...");
//     Ok(())
// }

// pub fn get_configuration<T>(request_uri_parts: Vec<String>) -> UriSection<T>
// where
//     T: Sync,
// {
//     UriSection {
//         execute_uri_section: Box::new(|request, stream, pool| {
//             Box::pin(async move { test(request, stream, pool).await })
//         }),
//         next_section: {
//             let uri_part_str;
//             if let Some(uri_part) = request_uri_parts.get(0) {
//                 uri_part_str = uri_part.as_str();
//             } else {
//                 uri_part_str = "_";
//             }
//             match uri_part_str {
//                 "project" => Some(Box::new(get_project_handler())),
//                 _ => None,
//             }
//         },
//     }
// }

// fn get_project_handler<T>() -> UriSection<T>
// where
//     T: Sync,
// {
// }

// async fn add_project(pool: &Arc<Pool<Postgres>>) {
//     pool.execute(
//         "INSERT INTO projects (name, description) VALUES ('New Project', 'Project Description')",
//     )
//     .await;
// }

// let uri_part_str;
// if let Some(uri_part) = request_uri_parts.get(0) {
//     uri_part_str = uri_part.as_str();
// } else {
//     uri_part_str = "_";
// }
// match uri_part_str {
//     "add" => Ok(Some(get_add_project_handler())),
//     _ =>  Ok(None),
// };
