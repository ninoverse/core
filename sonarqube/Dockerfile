FROM sonarsource/sonar-scanner-cli:latest

USER root

# Define explicit paths for Rust
ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH="/opt/cargo/bin:${PATH}"

# 1. Install system dependencies
# 2. Create the empty directories
# 3. Chown the EMPTY directories (instantaneous)
RUN dnf install -y gcc gcc-c++ make && \
    dnf clean all && \
    mkdir -p /opt/rustup /opt/cargo && \
    chown scanner-cli:scanner-cli /opt/rustup /opt/cargo

# Switch to the scanner user BEFORE installing Rust
USER scanner-cli

# Install Rust directly as the scanner-cli user.
# Since the user already owns /opt/rustup and /opt/cargo, no chown is needed!
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path && \
    rustup component add clippy