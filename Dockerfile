FROM openclaw
ARG UID=1000
ARG GID=1000
ARG USER=node
ENV UID=$UID
ENV GID=$GID
ENV USER=$USER

ENV TERM=xterm-256color
ENV PATH="/home/node/nodeenv/bin:$PATH"
ENV CLAWSHELL_OAUTH_CALLBACK_HOST=0.0.0.0
ENV CLAWSHELL_SERVER_HOST=0.0.0.0
EXPOSE 18790 51121
COPY target/release/clawshell /usr/local/bin/clawshell
USER node
WORKDIR /home/node
COPY .env /home/node/.env
ENTRYPOINT ["sudo", "-E", "env", "CLAWSHELL_OAUTH_CALLBACK_HOST=0.0.0.0", "PATH=/home/node/nodeenv/bin:/home/node/nodeenv/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin", "clawshell"]
