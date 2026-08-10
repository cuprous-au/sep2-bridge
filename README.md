# sep2-bridge
A bridge service that translates IEEE 2030.5 (SEP2) messages to and from external energy-system protocols and device interfaces.

Specifically, this is a Linux-based service that acts as a CSIP-AUS client to receive DNSP site limits.
These site limits will convey site meter data via Sunspec MODBUS profiles.

For instructions on how to run the service, please [go here](crates/sep2-bridge-service)