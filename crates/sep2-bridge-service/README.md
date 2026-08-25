sep2-bridge-service
===

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

Running the script in `examples/create_envoy_controls.sh` will setup a second
Site Control Group (DERProgram) in addition to the default one created by envoy,
and assign defaults to the program and its controls. Note that you will need to
have already started the client so that the device is registered before running
this script.

After creating these controls in envoy you should see something along the lines
of (with trace logging enabled):
```
[... DEBUG sep2_bridge::poll_handlers] Polled DERControlList, returned 2/2 items.
[... TRACE sep2_bridge::sep2_model] Program 20000000000000010000000100000000 (ContractedPremisesServiceProvider): num controls (after filter by status) (after all filtering) (+ default): 0 (0) (0) (+ 1)
[... TRACE sep2_bridge::sep2_model] Program 20000000000000020000000100000000 (InHomeEnergyManagementSystem): num controls (after filter by status) (after all filtering) (+ default): 2 (2) (2) (+ 0)
[... TRACE sep2_bridge::scheduler] Would have applied setpoint with 5 attributes
[... TRACE sep2_bridge::sep2_model] Program 20000000000000010000000100000000 (ContractedPremisesServiceProvider): num controls (after filter by status) (after all filtering) (+ default): 0 (0) (0) (+ 1)
[... TRACE sep2_bridge::sep2_model] Program 20000000000000020000000100000000 (InHomeEnergyManagementSystem): num controls (after filter by status) (after all filtering) (+ default): 2 (2) (2) (+ 0)
[... TRACE sep2_bridge::scheduler] Schedule next time: number of controls (active): 3 (2)
```
