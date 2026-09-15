# sep2-bridge

A bridge service that translates IEEE 2030.5 (SEP2) messages to and from external
energy-system protocols and device interfaces.

Specifically, this is a Linux-based service that acts as a CSIP-AUS client to
receive DNSP site limits and apply them to an inverter or site controller over
SunSpec Modbus, while sending that device's state and site meter data back to
the CSIP-AUS server.

Traffic flows in both directions:

- **Down (server to device).** Polls the CSIP-AUS server for assigned parameters
  in defaults and scheduled controls, layers them by primacy to work out which
  values apply right now, and writes the resulting parameters into the SunSpec
  Modbus device.
- **Up (device to server).** Reads status, settings, capability and meter
  readings from the device over Modbus and posts them back, along with
  responses acknowledging each control as it starts, ends, is cancelled or is
  superseded.

This service acts as a client to both the CSIP-AUS server and the SunSpec Modbus
device. The device side can use a TCP or a Unix socket connection (serial RTU
will be supported in the future).

The parameters translated between CSIP-AUS and the SunSpec protocols follow the
mappings in the draft Australian Standards document AS5438. See
[AS5438_comments](crates/sep2-bridge-service/docs/AS5438_comments.md) for
further information regarding the choices made in implementing that spec.

## Requirements

- A Rust toolchain.
- OpenSSL development libraries and `pkg-config`, as the SEP2 client is built on
  `hyper-openssl`.

## How to use

A minimal invocation needs the server to talk to, the credentials to talk to it
with, and the address of the SunSpec Modbus device:

```
cargo run -- \
  --server-addr csipaus.server.upstream \
  --credentials-directory /etc/sep2-bridge/credentials \
  --ca-path /etc/sep2-bridge/ca.crt \
  --modbus-socket unix:///tmp/modbus.sock
```

Note that TLS will not work out of the box on a current OpenSSL — see
[Credentials and TLS](#credentials-and-tls) below.

| Option                    | Default                   | Notes                                                                                                     |
|---------------------------|---------------------------|-----------------------------------------------------------------------------------------------------------|
| `--credentials-directory` | *(required)*              | Directory holding `client.crt`, `client.key` and optionally `registration_pin`                            |
| `--modbus-socket`         | *(required)*              | `tcp://<ip>[:<port>]` or `unix:///path/to.sock`              |
| `--ca-path`               | `/etc/sep2-bridge/ca.crt` | CA certificate for server verification |
| `--server-addr`           | `127.0.0.1:8080`          | The CSIP-AUS server address.                                                                            |
| `--dcap-uri`              | `/dcap`                   | Path of the `DeviceCapability` entry point of the CSIP-AUS server                                         |
| `--modbus-device-id`      | `1`                       | Modbus unit id, to distinguish devices sharing a connection                                               |
| `--max-list-size`         | `30`                      | Pagination limit used when querying list resources. |
| `--default-poll-rate`     | `900`                     | Seconds. Used only for resources where the server specifies no poll rate |
| `--pen`                   | `0`                       | Private Enterprise Number, used to make generated mRIDs unique                                            |
| `--metrics-url`           | None                      | If given, a unix socket path to push metrics (`unix:///path/to.sock`) |
| `--metrics-interval-sec`  | `60`                      | How often to push metrics to the metrics URL |

## Credentials and TLS

The credentials directory is expected to contain:

```
<credentials-directory>/
  client.crt          # client certificate (required)
  client.key          # matching private key (required)
  registration_pin    # optional, a decimal PIN
```

The device's LFDI and SFDI are derived from `client.crt`, so that certificate is what
identifies this device to the utility server. On startup the service fetches the
server's `EndDeviceList`: if the device is not there it registers itself, and if it is
there it checks that the entry really is this device.

If `registration_pin` is present, its contents are parsed as a number and compared
against the PIN in the server's `Registration` resource; a mismatch is logged as an
error and retried, since it usually means the device was commissioned with the wrong
PIN. The file is optional, and if not present the server's PIN will be ignored.

The CA certificate used to validate the *server* is separate, given by `--ca-path`.

### OpenSSL cipher configuration

CSIP-AUS mandates the `ECDHE-ECDSA-AES128-CCM8` cipher suite, which current versions
of OpenSSL will not negotiate at their default security level. **Without the
configuration below the TLS handshake simply fails**, and the cause is not obvious
from the error. Write an OpenSSL config:

```ini
openssl_conf = default_conf

[default_conf]
ssl_conf = ssl_sect

[ssl_sect]
system_default = system_default_sect

[system_default_sect]
CipherString = ECDHE-ECDSA-AES128-CCM8:@SECLEVEL=0
```

and point OpenSSL at it when running the service:

```
export OPENSSL_CONF=/etc/sep2-bridge/openssl.cnf
```

## Logging

Logging goes to stderr via `env_logger`, with millisecond timestamps, and is
controlled by `RUST_LOG`:

```
export RUST_LOG=sep2_bridge=info
```

## Tests

The tests in `crates/sep2-bridge-service/tests/` exercise each task in isolation,
using `wiremock` to stand in for the SEP2 server and a mock Modbus server for the
device side, plus a set of end-to-end translation tests for the AS5438 parameter
mappings.

## Troubleshooting

A common issue that is hard to diagnose is the error message:
```
[... ERROR sep2_bridge::sep2_connection] Failed trying to get dcap: error trying to connect:
error:0A000410:SSL routines:ssl3_read_bytes:ssl/tls alert handshake failure:ssl/record/rec_layer_s3.c:918
:SSL alert number 40
```
This message is the OpenSSL library failing to negotiate with the upstream
CSIP-AUS server. This will happen if the cipher configuration is incorrect,
please make sure you have set up your environment as described in [OpenSSL
cipher configuration](#credentials-and-tls).

If you still receive the error message with that configuration, please check
certificates and other OpenSSL details. You might like to attempt to curl the
endpoint directly yourself with the client certificates to debug the issue
further.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).
