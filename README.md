# Mitosis: A Unified Transport Evaluation Framework

## Usage

### Coordinator

#### Dependencies

pkg-config, libssl-dev

#### Generate keys

```bash
openssl genpkey -algorithm ed25519 -out private.pem
openssl pkey -in private.pem -pubout -out public.pem
```

#### Set .env file

Copy `.env.example` to `.env` and set the variables in it.

Configuring all the relevant options in config.toml for coordinator and run the following command:

```bash
mito coordinator --file-log
```

### Worker

Configuring all the relevant options in config.toml for worker and run the following command:

```bash
mito worker --file-log
```
