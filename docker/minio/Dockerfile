# Local development/test images only. Source provenance: README.md in this directory.
FROM golang:1.24.7-alpine3.22@sha256:fc2cff6625f3c1c92e6c85938ac5bd09034ad0d4bc2dfb08278020b68540dbb5 AS builder
RUN apk add --no-cache curl
ENV CGO_ENABLED=0 GOTOOLCHAIN=local
WORKDIR /src

FROM builder AS minio-build
RUN curl -fL --retry 3 https://codeload.github.com/minio/minio/tar.gz/07c3a429bfed433e49018cb0f78a52145d4bedeb -o /source.tar.gz \
    && echo '8819e3e7817e46b7b3798f8f200ead208562e571563c2e040352378031abe9f2  /source.tar.gz' | sha256sum -c - \
    && tar -xzf /source.tar.gz --strip-components=1
RUN go build -mod=readonly -trimpath -buildvcs=false -o /out/minio .

FROM builder AS mc-build
RUN curl -fL --retry 3 https://codeload.github.com/minio/mc/tar.gz/7394ce0dd2a80935aded936b09fa12cbb3cb8096 -o /source.tar.gz \
    && echo '95cd293c7119f16921a6dc515a1fb74a2227f19fd994b9c8b770a154e802ac44  /source.tar.gz' | sha256sum -c - \
    && tar -xzf /source.tar.gz --strip-components=1
RUN go build -mod=readonly -trimpath -buildvcs=false -o /out/mc .

FROM alpine:3.22.1@sha256:4bcff63911fcb4448bd4fdacec207030997caf25e9bea4045fa6c8c44de311d1 AS runtime
# curl is required by the existing Compose healthcheck; /bin/sh by minio-init.
RUN apk add --no-cache ca-certificates curl

FROM runtime AS minio
LABEL org.opencontainers.image.source="https://github.com/minio/minio" \
      org.opencontainers.image.revision="07c3a429bfed433e49018cb0f78a52145d4bedeb" \
      org.opencontainers.image.licenses="AGPL-3.0-only"
COPY --from=minio-build /out/minio /usr/bin/minio
COPY --from=minio-build /src/LICENSE /usr/share/doc/minio/LICENSE
COPY --from=minio-build /source.tar.gz /usr/share/doc/minio/source.tar.gz
EXPOSE 9000 9001
ENTRYPOINT ["/usr/bin/minio"]

FROM runtime AS mc
LABEL org.opencontainers.image.source="https://github.com/minio/mc" \
      org.opencontainers.image.revision="7394ce0dd2a80935aded936b09fa12cbb3cb8096" \
      org.opencontainers.image.licenses="AGPL-3.0-only"
COPY --from=mc-build /out/mc /usr/bin/mc
COPY --from=mc-build /src/LICENSE /usr/share/doc/mc/LICENSE
COPY --from=mc-build /source.tar.gz /usr/share/doc/mc/source.tar.gz
ENTRYPOINT ["/usr/bin/mc"]
