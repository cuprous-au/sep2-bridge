#!/bin/bash

# This script is intended to be run from a fresh envoy server start with a single registered end device. It requires (and makes heavy assumptions) on an end device already having registered itself. The typical run steps are:
# 1. Start envoy's demo with a blank DB and run envoy's `reset.sh`.
# 2. Start the sep2-bridge to connect and register an end device.
# 3. Run this script.

# Create Site Control Group
curl -X POST \
  -i \
  http://127.0.0.1:8001/site_control_group \
  --user 'admin:password' \
  --json '{
  "description": "cntl 1",
  "primacy": 5,
  "fsa_id": 1
}'

# Inspect the output. You should see a 201 response with location `/site_control_group/2`.

# Create a site group
curl -X POST -i \
  http://127.0.0.1:8001/site_group \
  --user 'admin:password' \
  --json '{
  "name": "test_group",
  "default_group": true
}'

# You should see a 201 with location `/site_group/test_group`

# Assign the site to the site group
curl -X POST -i \
  http://127.0.0.1:8001/site_group/test_group/assignments \
  --user 'admin:password' \
  --json '{
  "site_id": 1
}'

# Create a set point
# 
curl -X POST -i \
  http://127.0.0.1:8001/site_control_group/2/controls \
  --user 'admin:password' \
  --json '[
  {
    "site_group_id": 1,
    "calculation_log_id": null,
    "duration_seconds": 300,
    "start_time": "2026-08-26T00:30:00Z",
    "import_limit_watts": 5000
  }
]'
