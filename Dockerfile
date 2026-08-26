FROM rust:1.95.0-slim AS builder
WORKDIR /app

# Install build dependencies if needed (e.g., for ring/rustls)
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

# Copy the source code
COPY . .

# Build the application with the rig-worker feature enabled (optional but usually what you want for demo)
RUN cargo build --release --locked --features rig-worker

# Final minimal runtime image
FROM debian:bookworm-slim
WORKDIR /app

# Install runtime dependencies (like ca-certificates for outgoing HTTPS requests to Gemini)
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

# Copy the binary from builder
COPY --from=builder /app/target/release/meld /app/meld

# Expose the default port Render will inject
EXPOSE 3000

# Run the binary
CMD ["/app/meld"]
