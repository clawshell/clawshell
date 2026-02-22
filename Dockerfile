FROM debian:sid-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
ENV TERM=xterm-256color
EXPOSE 18790
WORKDIR /etc/clawshell
COPY target/release/clawshell /usr/local/bin/clawshell
COPY .env .env
ENTRYPOINT ["clawshell"]
