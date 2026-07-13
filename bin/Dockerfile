FROM rust:bullseye AS builder
RUN apt-get update 
RUN apt-get install -y cmake
WORKDIR /usr/src/ninoverse
COPY . .
RUN cargo install --path .

FROM debian:bullseye-slim
WORKDIR /usr/src/ninoverse
COPY --from=builder /usr/local/cargo/bin/ninoverse .
COPY --from=builder /usr/src/ninoverse/sql ./sql
EXPOSE 7878
CMD ["./ninoverse"]