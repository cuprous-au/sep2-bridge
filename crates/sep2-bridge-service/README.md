sep2-bridge-service
===

## Alignment with AS5438

This crate is strongly motivated by the draft spec of AS5438 and is aimed at
supporting devices that implement all of the control parameters mentioned in the
tables of that spec.

In order to correctly send those parameters to a SunSpec modbus compatible
device, several other parameters not mentioned in AS5438 are also required. The
details of the changes made to be compatible with AS5438 are described in
[AS5438_comments](docs/AS5438_comments.md).

## Testing with envoy

Envoy is a CSIP-AUS server developed by the BSGIP at ANU. We can use it here to
test the sep2-bridge's CSIP-AUS client behaviour. To setup a test environment:

1. First get envoy up and running. Clone it:
```
git clone https://github.com/bsgip/envoy
```
2. Bring its docker compose environment up
```
cd demo
HOST_UID=$(id -u) HOST_GID=$(id -g) docker compose up --build
```

For the sep2-bridge itself, the main entrypoint serves as a test currently. You
should initially set up a set of configuration files for testing:

1. Initialise the environment to set up keys and OpenSSL (note that OpenSSL is required to be installed!):
> Be sure to update "<path to envoy/demo/tls-termination/test_certs>"
```
export CREDENTIALS_DIRECTORY=<path to envoy/demo/tls-termination/test_certs>
ln -s ${CREDENTIALS_DIRECTORY}/{testdevice1,client}.crt
ln -s ${CREDENTIALS_DIRECTORY}/{testdevice1,client}.key
export CA_PATH="${CREDENTIALS_DIRECTORY}/testca.crt"
export SEP2_OPENSSL_CIPHER_STRING='ECDHE-ECDSA-AES128-CCM8:@SECLEVEL=0'

cat >/tmp/sep2-openssl.cnf <<EOF
openssl_conf = default_conf

[default_conf]
ssl_conf = ssl_sect

[ssl_sect]
system_default = system_default_sect

[system_default_sect]
CipherString = ${SEP2_OPENSSL_CIPHER_STRING}
EOF

export OPENSSL_CONF=/tmp/sep2-openssl.cnf
```

2. Run the sep2-bridge using the certificates that envoy has generated.
```
RUST_LOG=sep2_bridge=info \
cargo run --bin sep2-bridge -- \
  --server-addr 127.0.0.1:8443 \
  --default-poll-rate 15 \
  --modbus-socket=unix:///tmp/a.sock
```

What will the test do?

- gets the device capabilities.
- checks if its device (known by the certificate at `CERT_PATH`) is known to the server
- registers its device with the server if unknown
- registers a poll on the device list with an overridden poll period of 15s.

Envoy is bootstrapped with several certificates in its registry of known LFDIs.
This is much like registering the devices out of band. The sep2-bridge client is
using the envoy "testdevice1" certificate. On first run the server should
respond with no known device, but allow the client to register itself. On second
run the server will respond with the client certificate already known.

Envoy by default has no default controls or scheduled controls created for this
device. While running the client you should be able to set these up and have the
server inform the client of new defaults and new controls.

First assign the bridge client to a site group:
```
# Create a site group
curl -X POST -i --user admin:password \
  http://127.0.0.1:8001/site_group \
  --json '{
  "name": "test_group",
  "default_group": false
}'

# You should see a 201 response with location `/site_group/test_group`

# Assign the site to the site group
curl -X POST -i --user admin:password \
  http://127.0.0.1:8001/site_group/test_group/assignments \
  --json '{"site_id": 1}'
```

To then apply a control to the default control group, that will take immediate
effect for the next 5 minutes, you can send:
```
NOW=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

curl -X POST -i --user admin:password \
  http://127.0.0.1:8001/site_control_group/1/controls \
  --json '[
  {
    "site_group_id": 1,
    "calculation_log_id": null,
    "duration_seconds": 300,
    "start_time": "'$NOW'",
    "set_point_percentage": -50
  }
]'
```

Note that in the above, the set point is expressed as a negative value to indicate
that it is a limit on power consumed, not generated.

Other parameters can be changed in envoy to set more parameters, add site
control groups (mapping to DERPrograms) and add default controls (mapping to DefaultDERControls).
