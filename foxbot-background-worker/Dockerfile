FROM rust:1.52-slim-buster AS builder
WORKDIR /src
COPY ./foxbot-background-worker/foxbot-background-worker ./foxbot-background-worker
RUN strip ./foxbot-background-worker

FROM registry.huefox.com/syfaro/foxbot/base
COPY ./langs ./langs
COPY --from=builder /src/foxbot-background-worker /bin/foxbot-background-worker
CMD ["/bin/foxbot-background-worker"]
