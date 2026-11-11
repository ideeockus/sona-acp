# The environment of one Run: Pi on Node, nothing else (D61).
FROM node:22-slim@sha256:5ec23b7c622b00547753bc6ffd78085dd868a68aeb41c8e0a1418f033e13d96d

RUN npm install --global @earendil-works/pi-coding-agent@1.0.0 \
 && npm cache clean --force

# Unprivileged; Agent Runtime mounts /work and /agent/* at run time (D83).
RUN useradd --create-home --uid 10001 agent
USER agent
WORKDIR /work

ENTRYPOINT ["pi"]
