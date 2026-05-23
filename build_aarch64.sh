#!/usr/bin/env bash
# ==============================================================================
# Script to build and extract a static ARM64 (Kunpeng) binary using Docker
# ==============================================================================
set -euo pipefail

# Ensure we are in the project root
cd "$(dirname "$0")"

echo "==> 1. Building the Docker image containing the statically compiled ARM64 binary..."
docker build -t xtrace-aarch64-builder -f Dockerfile.aarch64 .

echo "==> 2. Creating a temporary container..."
docker create --name xtrace-temp-container xtrace-aarch64-builder

echo "==> 3. Extracting the compiled static binary to the local directory..."
docker cp xtrace-temp-container:/app/xtrace ./xtrace-aarch64

echo "==> 4. Cleaning up temporary containers..."
docker rm xtrace-temp-container

echo "=============================================================================="
echo "==> SUCCESS! Statically compiled ARM64 binary is saved at ./xtrace-aarch64"
echo "=============================================================================="
ls -lh ./xtrace-aarch64
