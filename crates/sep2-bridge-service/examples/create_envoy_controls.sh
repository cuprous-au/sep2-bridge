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

# Set defaults on pre-existing control group
curl -X POST -i \
  http://127.0.0.1:8001/site_control_group/1/default \
  --user 'admin:password' \
  --json '{
  "import_limit_watts": {
    "value": 500
  },
  "export_limit_watts": {
    "value": 500
  },
  "generation_limit_watts": {
    "value": 500
  },
  "load_limit_watts": {
    "value": 500
  },
  "ramp_rate_percent_per_second": {
    "value": 500
  }
}'

# Set defaults on new control group
curl -X POST -i \
  http://127.0.0.1:8001/site_control_group/2/default \
  --user 'admin:password' \
  --json '{
  "import_limit_watts": {
    "value": 42
  },
  "export_limit_watts": {
    "value": 42
  },
  "generation_limit_watts": {
    "value": 42
  },
  "load_limit_watts": {
    "value": 42
  },
  "ramp_rate_percent_per_second": {
    "value": 42
  }
}'

# Create a calculation log (necessary for a control)
curl -X POST -i \
  http://127.0.0.1:8001/calculation_log \
  --user 'admin:password' \
  --json '{
  "calculation_range_start": "2010-01-02T00:00:01Z",
  "calculation_range_duration_seconds": 2,
  "interval_width_seconds": 300,
  "variable_metadata": [{"variable_id": 1, "name": "variable 1", "description": "aaa"}],
  "variable_values": {"variable_ids": [1], "site_ids": [1], "interval_periods": [1], "values": [1.24]},
  "label_metadata": [{"label_id": 6, "name": "nice label", "description": "nice"}],
  "label_values": {"label_ids": [6], "site_ids": [1], "values": ["aa"]}
}'

# You should see a 201 with location `/calculation_log/1`

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

curl -X POST -i \
  http://127.0.0.1:8001/site_control_group/2/controls \
  --user 'admin:password' \
  --json '[
  {
    "site_group_id": 1,
    "calculation_log_id": 1,
    "duration_seconds": 300,
    "start_time": "2026-08-10T00:10:00Z",
    "import_limit_watts": 500
  },
  {
    "site_group_id": 1,
    "calculation_log_id": 1,
    "duration_seconds": 300,
    "start_time": "2026-08-11T00:10:00Z",
    "import_limit_watts": 500
  }
]'
