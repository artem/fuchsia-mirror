# TEE

This directory contains support for hosting a Trusted Execution Environment inside Fuchsia.

## Glossary

* TEE - Trusted Execution Environment. This is an environment suitable for
executing a TA that should be isolated from less trusted software.

* TA - Trusted Application. Program which executes within a TEE and which may
have access to sensitive resources such as cryptographic keys. A TA performs
computations using these resources on behalf of its client.

* REE - Rich Execution Environment. General purpose computing environment that may contain
less trusted data and software.

* TEE Client API - API used by programs running in the REE to communicate with TAs.

* TEE Internal Core API - API exposed to TAs running in the TEE.

## References

[TEE Client API implementation](//src/security/lib/tee)
[OP-TEE documentation of the APIs and extensions supported by their implementation](https://optee.readthedocs.io/en/latest/architecture/globalplatform_api.html#introduction)
