# Changelog

All notable changes to this project will be documented in this file.

## [0.6.1](https://github.com/stack-rs/mitosis/compare/mito-v0.6.0...mito-v0.6.1) - 2025-09-14

### Features

- *(main)* Add entrypoint for manager - ([8465879](https://github.com/stack-rs/mitosis/commit/8465879a1b3ab1c53c8993bf14a94b71c00d522b))


## [0.6.0](https://github.com/stack-rs/mitosis/compare/mito-v0.5.3...mito-v0.6.0) - 2025-09-11

### Features

- *(api)* Support labels of worker - ([72a123a](https://github.com/stack-rs/mitosis/commit/72a123a746d12ad56642be69e1102248aa063e93))
- *(api)* Comply endpoints to OpenAPI spec - ([4ed5ef0](https://github.com/stack-rs/mitosis/commit/4ed5ef074243e584af0b0d803fa5cf3752cf0bca))
- *(db)* Add database query indices for performance - ([51eb073](https://github.com/stack-rs/mitosis/commit/51eb0734d5a1b74cc54c8560b521b6d2f687d850))
- *(db)* Add database performance indices - ([b482224](https://github.com/stack-rs/mitosis/commit/b482224a07a21132a852b6d0440bf9f94fe71d70))
- Support different underlying unbounded channels - ([b87bada](https://github.com/stack-rs/mitosis/commit/b87badae86ae21d9f3326331900911263e26d154))

### Documentation

- *(guide)* Update key concepts and usage guide - ([784938b](https://github.com/stack-rs/mitosis/commit/784938b24dcbedbcb94729804c1561ecb160c074))
- *(spec)* Add current OpenAPI specification - ([d222a08](https://github.com/stack-rs/mitosis/commit/d222a0886b2b83c16e85cc521e4249f009996c5e))


## [0.5.3](https://github.com/stack-rs/mitosis/compare/mito-v0.5.2...mito-v0.5.3) - 2025-09-03

### Features

- *(conf)* Allow worker to skip redis - ([3c2484f](https://github.com/stack-rs/mitosis/commit/3c2484fe72645cb612dad253e8ddce9f3567423f))
- *(redis)* Support skip setting ACL rules - ([ce09bd8](https://github.com/stack-rs/mitosis/commit/ce09bd861e0b97eb3b1449712c0c947aa8743ccd))
- *(s3)* Support enterprise-level object storage - ([7fc4bde](https://github.com/stack-rs/mitosis/commit/7fc4bde5722baf5bab4085d71d95b3aaa7c33405))

### Bug Fixes

- *(conf)* Skip serializing false boolean fields - ([47a4564](https://github.com/stack-rs/mitosis/commit/47a4564ced6d6241eb64bf51308bba8edd6f7bb5))
- *(s3)* Allow empty file with size 0 - ([ac6cb91](https://github.com/stack-rs/mitosis/commit/ac6cb9114e4a3527b7aa74ae946b7a026ff4456e))

### Refactor

- *(client)* [**breaking**] Rename downloading methods - ([5cc38d2](https://github.com/stack-rs/mitosis/commit/5cc38d224fe61cf8c8f8a5e4ec27a615897f7988))

### Documentation

- *(guide)* Add installation steps for musl release - ([9fc370a](https://github.com/stack-rs/mitosis/commit/9fc370a1d303653071b2a7c1660bfbc15485bf98))

### Miscellaneous Tasks

- Build gh-pages on release for version update - ([55a836a](https://github.com/stack-rs/mitosis/commit/55a836acb22eabfc71c36519e9cb35d5adcb686c))

### Build

- Add long version metadata - ([239612d](https://github.com/stack-rs/mitosis/commit/239612d8f86557ef177a130fc8f1d8c4e1e90141))

## [0.5.2](https://github.com/stack-rs/mitosis/compare/mito-v0.5.1...mito-v0.5.2) - 2025-08-26

### Features

- *(admin)* [**breaking**] Remove request body for user deletion - ([ddfaa05](https://github.com/stack-rs/mitosis/commit/ddfaa05a454ae79bd3e1d339e2768e533724a111))

## [0.5.1](https://github.com/stack-rs/mitosis/compare/mito-v0.5.0...mito-v0.5.1) - 2025-08-26

### Build

- *(dist)* Add runner for x86_64-unknown-linux-musl - ([7ab5be7](https://github.com/stack-rs/mitosis/commit/7ab5be7ae98633f4e85869e4f7a085d00a76e53d))

## [0.5.0](https://github.com/stack-rs/mitosis/compare/mito-v0.4.0...mito-v0.5.0) - 2025-08-26

### Features

- *(api)* Support admin change group storage quota - ([c7c8d60](https://github.com/stack-rs/mitosis/commit/c7c8d60aa8c13eb80c8c3d08c482381f7231c4a2))
- *(api)* Support change password - ([9a52468](https://github.com/stack-rs/mitosis/commit/9a524680ab1b038f220ee51fa98904f1f413b900))
- *(api)* Return username for auth interface - ([2bb8999](https://github.com/stack-rs/mitosis/commit/2bb899949b1b88186571486d1056c3e482c8e8d9))
- *(api)* Support delete artifact and attachment - ([332998f](https://github.com/stack-rs/mitosis/commit/332998ff002e052c76c7561b5799d391f1389952))
- *(api)* Support complex query for tasks - ([7c0564d](https://github.com/stack-rs/mitosis/commit/7c0564df838bc98edb12b61a8fdfd73f9699d36d))
- *(api)* Support query counts only for attachments - ([520aca7](https://github.com/stack-rs/mitosis/commit/520aca76afb3fc55ab82104834ea5ed44329faf7))
- *(api)* Support query counts only for workers - ([5ae022b](https://github.com/stack-rs/mitosis/commit/5ae022b0bc37c67a9888ea7e7d7584ede18dd597))
- *(api)* Allow CORS - ([2caf034](https://github.com/stack-rs/mitosis/commit/2caf034119d640de31fecff4d80e08933cbb99a5))
- *(auth)* Allow user to retain previous login - ([6593fe4](https://github.com/stack-rs/mitosis/commit/6593fe488a78d1ec5b57bbdf83760d961e7803af))
- *(conf)* Add default value for ClientConfig - ([7e4a247](https://github.com/stack-rs/mitosis/commit/7e4a247f79bc53e2e24b3a5cbee64b1a5066a639))
- *(conf)* Support read global config file - ([bc552dd](https://github.com/stack-rs/mitosis/commit/bc552ddc4e53971837c7fd89df56750b44967157))
- *(s3)* Allow more time for uploading - ([75fd9ea](https://github.com/stack-rs/mitosis/commit/75fd9ea3d589f1b66961f186e7eaad9d5b99b637))
- *(sdk)* Support progress bar for file operation - ([65dcd81](https://github.com/stack-rs/mitosis/commit/65dcd812916f1638d486f11128be6ee0d0299eb5))
- *(sdk)* Add smart parse for file path arguments - ([ed07a62](https://github.com/stack-rs/mitosis/commit/ed07a62a65626367abc43744aa06d212bf3b2498))
- *(sdk)* Auto-fill the omitted file name on uploading - ([679d833](https://github.com/stack-rs/mitosis/commit/679d833faf863e66df2c09834742defeede56f68))
- *(sdk)* Support directly run external commands - ([e917fad](https://github.com/stack-rs/mitosis/commit/e917fad2611df93a2ded8136386b83b5164aff16))

### Bug Fixes

- *(api)* Add middleware to redis interface - ([a6bfc7f](https://github.com/stack-rs/mitosis/commit/a6bfc7f370fc61213783a40457941b18909c44b1))
- *(worker)* Try to mitigate heartbeat update deadlock - ([0e7116c](https://github.com/stack-rs/mitosis/commit/0e7116c7c4be82a6507fd3dc948d19eac0010154))
- Sync routes in different feature flags - ([32ffc7e](https://github.com/stack-rs/mitosis/commit/32ffc7e70e6771dfba3c34055bd88fb428b395a2))

### Refactor

- *(task)* Get group name before permission check - ([c1b1c05](https://github.com/stack-rs/mitosis/commit/c1b1c059f9f4ce892803a9b996e7ee6b7a6058c9))
- [**breaking**] Reorganize api interfaces and client sdk - ([4725e24](https://github.com/stack-rs/mitosis/commit/4725e2417d64c1eb286eb9483f3ec2852608a863))

### Documentation

- *(example)* Add retain option to example config - ([56b1219](https://github.com/stack-rs/mitosis/commit/56b1219a7efb5f4d38bd1e7241aeefc57548c8d8))
- *(guide)* Update usage guidance - ([046bff2](https://github.com/stack-rs/mitosis/commit/046bff234440a54ea276a347e10acc44169d73cc))
- Fix wrong comments - ([de8e713](https://github.com/stack-rs/mitosis/commit/de8e7137fb7a8b0e019e30064ce17667d769577b))

### Styling

- Format docker-compose.yml - ([4798760](https://github.com/stack-rs/mitosis/commit/4798760544970f81470598204f47309f65f919b1))
- Fix clippy warnings - ([f2f5342](https://github.com/stack-rs/mitosis/commit/f2f53429312e78fa562c726d7d38e20bf35e6845))

### Miscellaneous Tasks

- Allow build CI to run from fork's PR - ([0601b0c](https://github.com/stack-rs/mitosis/commit/0601b0c4457924a7d3b6ccc0f4d92382cdf26eae))
- Replace latest version on docs build - ([2f74600](https://github.com/stack-rs/mitosis/commit/2f74600706de46e7d8a518d280b30f73863a5e38))
- Downgrade fetch task tracing level - ([94797a9](https://github.com/stack-rs/mitosis/commit/94797a905f9e2852fcf9e7cdc909fed16dec96cf))
- Re-export some third-party libraries - ([634b3d6](https://github.com/stack-rs/mitosis/commit/634b3d600782e2a53a393c7092a8d8a8018610dd))
- Impl clone trait for schema data structure - ([c1f3e3c](https://github.com/stack-rs/mitosis/commit/c1f3e3c285afd9c0ea245aa295018f055840036f))

### Build

- *(dist)* Replace native-tls with rustls - ([c36f5c8](https://github.com/stack-rs/mitosis/commit/c36f5c89d04dea67f411a22ab893668ed7b98a3e))
- *(dist)* Add new target x86_64-unknown-linux-musl - ([48aab5e](https://github.com/stack-rs/mitosis/commit/48aab5e0d407c037ddfc8cd5d34a34b148a3d8a0))

## [0.4.0](https://github.com/stack-rs/mitosis/compare/mito-v0.3.2...mito-v0.4.0) - 2025-07-22

### Features

- *(skd)* Support no-download of artifact - ([0f10dc4](https://github.com/stack-rs/mitosis/commit/0f10dc4a9cb01e0a1f891ea5985ec0d3e0c2c0be))
- *(worker)* Support specify group role on register - ([3226def](https://github.com/stack-rs/mitosis/commit/3226def828433ed107f9d57fd0e8daac2e04918b))

### Documentation

- *(guide)* Prepare for new release version - ([4bfb5c0](https://github.com/stack-rs/mitosis/commit/4bfb5c05342607fc398f9714a5e6c1651680d224))
- *(guide)* Add examples of usage - ([118c6ce](https://github.com/stack-rs/mitosis/commit/118c6ce4a2d25da35d438bb860872b9339323124))
- *(guide)* Update installation instructions - ([0d9ecff](https://github.com/stack-rs/mitosis/commit/0d9ecff0c2e65ccb774faaf7ffa90e05153e0a9c))

### Miscellaneous Tasks

- Update build conditions - ([950c1a8](https://github.com/stack-rs/mitosis/commit/950c1a8abfac1f96090413f4c0d2babdc4afaf59))

## [0.3.2](https://github.com/stack-rs/mitosis/compare/mito-v0.3.1...mito-v0.3.2) - 2025-07-08

### Miscellaneous Tasks

- *(dist)* Fix dist runner image - ([b76d7d0](https://github.com/stack-rs/mitosis/commit/b76d7d0658479e51d097c285ff54a5c2e37862e4))

## [0.3.1](https://github.com/stack-rs/mitosis/compare/mito-v0.3.0...mito-v0.3.1) - 2025-07-08

### Styling

- *(clippy)* Reduce large err-variant in result - ([d167c56](https://github.com/stack-rs/mitosis/commit/d167c56222839fbff7eeea1565effa80abeef2e7))
- Fix clippy warnings - ([db91735](https://github.com/stack-rs/mitosis/commit/db9173560e1273abba93be63d6673a47637bef43))

### Miscellaneous Tasks

- *(release)* Update dist workflow and config - ([a6ecfe3](https://github.com/stack-rs/mitosis/commit/a6ecfe3ea76259ed028b0818fb7d69b5ec72a1e6))

## [0.3.0](https://github.com/stack-rs/mitosis/compare/mito-v0.2.0...mito-v0.3.0) - 2025-01-31

### Features

- *(api)* Support admin shutdown coordinator - ([79bf8d1](https://github.com/stack-rs/mitosis/commit/79bf8d1853a9d97fd4f1e2e33245623130f12fe3))

### Bug Fixes

- *(client)* Change interact error message display - ([57e4023](https://github.com/stack-rs/mitosis/commit/57e40239a9bdbdecc4e43dce011534716d391af0))
- *(task)* Relax local path constraints in spec - ([1282f72](https://github.com/stack-rs/mitosis/commit/1282f72ba01302093a4d634120d6cc0c3efb14f7))

## [0.2.0](https://github.com/stack-rs/mitosis/compare/mito-v0.1.0...mito-v0.2.0) - 2024-11-15

### Features

- *(api)* Support attachment metadata query - ([c9d6637](https://github.com/stack-rs/mitosis/commit/c9d66377959c9d1472f8fc036a3e1c746be88940))
- *(client)* Enhance client prompt - ([9c39cfd](https://github.com/stack-rs/mitosis/commit/9c39cfd094a7754a7542d48263ddbe422de06e91))
- *(sdk)* Add attachment meta query to client - ([1537968](https://github.com/stack-rs/mitosis/commit/1537968d099e6650299eb9f54be0058465d11d33))

### Documentation

- Update installation guide - ([435f839](https://github.com/stack-rs/mitosis/commit/435f839f028a096e2c37781bfe8d1a2774252f41))

### Miscellaneous Tasks

- Change pr name of release-plz - ([9d94dcc](https://github.com/stack-rs/mitosis/commit/9d94dcc39024e12647d1b7a688aa8661240e234e))

## [0.1.0](https://github.com/stack-rs/mitosis/releases/tag/mito-v0.1.0) - 2024-11-05

### Features

- *(api)* Add worker count to group query result - ([3895387](https://github.com/stack-rs/mitosis/commit/38953870cad30a5dda00691912ccc35be60aadcb))
- *(api)* Add group info query interface - ([e0e9405](https://github.com/stack-rs/mitosis/commit/e0e940500d247b17420264a16c984ab05d0f8792))
- *(api)* Add group management interfaces - ([37d26a4](https://github.com/stack-rs/mitosis/commit/37d26a4a58cbedc409c84911d33454d58285c99d))
- *(api)* Add task management interfaces - ([f150694](https://github.com/stack-rs/mitosis/commit/f1506940576acc9d7436371a3138dd3dcf9340ed))
- *(api)* Add worker management interfaces - ([3a8ffe5](https://github.com/stack-rs/mitosis/commit/3a8ffe5b8c5a042d2f8b31f41f1c652b382fdb49))
- *(api)* Add worker info query interfaces - ([69a72af](https://github.com/stack-rs/mitosis/commit/69a72afdd7aea395f6d08e860ff50e73c5b81b2f))
- *(api)* Add api interface of coordinator - ([78ee7b1](https://github.com/stack-rs/mitosis/commit/78ee7b1798918908e96def05fe832b65f8dd441e))
- *(api)* Add basic user management api - ([b3cd873](https://github.com/stack-rs/mitosis/commit/b3cd87324ea33d51ff57617fd05470ad19247fa5))
- *(auth)* Return credential with username - ([8b8cc23](https://github.com/stack-rs/mitosis/commit/8b8cc23576494e64bd87a13b3177b54220f84069))
- *(client)* Add manual auth interface to sdk - ([c39e24a](https://github.com/stack-rs/mitosis/commit/c39e24a3195544ac91cecf60c3196b2be42ee79b))
- *(client)* Support only get attachment url - ([2a8d92d](https://github.com/stack-rs/mitosis/commit/2a8d92d67318c250df3f9d6eb8725e56434cffda))
- *(client)* Add sync pubsub redis client sdk - ([e235d80](https://github.com/stack-rs/mitosis/commit/e235d8040a4b798240b15ea20950a143c0e2a372))
- *(client)* Separate redis client sdk - ([8fe2b12](https://github.com/stack-rs/mitosis/commit/8fe2b128c5235b81b28236d117dd21b40b264e9d))
- *(client)* Add interface to download artifact - ([a5e5dd4](https://github.com/stack-rs/mitosis/commit/a5e5dd414ddc5735abc9976e5ee26f7021daf2b9))
- *(conf)* Add config for client and worker - ([ac06544](https://github.com/stack-rs/mitosis/commit/ac06544ea6dac01f6a862d49b0fa89904b733182))
- *(conf)* Add coordinator config builder - ([1dd1ef2](https://github.com/stack-rs/mitosis/commit/1dd1ef249da8325d5f18a4fb71020958a08b1b64))
- *(error)* Add new kinds of error - ([218adf7](https://github.com/stack-rs/mitosis/commit/218adf77cc535ba3e1f9f1b2d4cb93018489efdb))
- *(group)* Support users to query their groups - ([7fda3d3](https://github.com/stack-rs/mitosis/commit/7fda3d3e2e76111f70eee6f1691464737ecbf562))
- *(mito)* Add main entrypoint and file logger - ([965da17](https://github.com/stack-rs/mitosis/commit/965da17b4860eb57faf5aeedf659e26e93c380b6))
- *(mito)* Add entrypoint of worker - ([dfee517](https://github.com/stack-rs/mitosis/commit/dfee5173fb73b24180f9eb88d04300beb4db53fd))
- *(mito)* Add entrypoint of client - ([aa4f9f9](https://github.com/stack-rs/mitosis/commit/aa4f9f9fa26a7bdcdae6e84e7610c860a9e6265a))
- *(mito)* Add entrypoint of coordinator - ([55fcef3](https://github.com/stack-rs/mitosis/commit/55fcef3d9f7ff21afc913fa3f90b3f75e0e73009))
- *(redis)* Add async pubsub using RESP3 - ([85691fe](https://github.com/stack-rs/mitosis/commit/85691fe6f90255533d6af6a5e310b44f5e19dd8d))
- *(s3)* Ensure file flush after downloading - ([1280b96](https://github.com/stack-rs/mitosis/commit/1280b960c28048269b7593205d1c15bdf3f9b9f3))
- *(s3)* Add list query for attachments - ([0da6fff](https://github.com/stack-rs/mitosis/commit/0da6fffc4118de456bd4afcf75a6a38ea5e06a6c))
- *(s3)* Validate the key of the attachment - ([3965b99](https://github.com/stack-rs/mitosis/commit/3965b996eda45c911e419777029cf57e4fe6e5a5))
- *(s3)* Check roles for uploading attachment - ([f64138a](https://github.com/stack-rs/mitosis/commit/f64138ab0b4b281cdfcc61929b6a8c8120711723))
- *(s3)* Support attachment upload and download - ([4147bd7](https://github.com/stack-rs/mitosis/commit/4147bd7b5b840bea8b0ff29bdaadaf1531c6d84e))
- *(s3)* Limit valid duration for presigned url - ([7e14ec4](https://github.com/stack-rs/mitosis/commit/7e14ec474c07cc6f16e4f223aca17160298e3bcd))
- *(schema)* Impl FromRedisValue for ExecState - ([ab8b388](https://github.com/stack-rs/mitosis/commit/ab8b3885c4504759ee88c92e9361d4a1be9be6c2))
- *(schema)* Add watch for state of other tasks - ([b3c4cc6](https://github.com/stack-rs/mitosis/commit/b3c4cc6dcdbfaeee962454c9d5143a0eb83efab0))
- *(schema)* Add labels field to tasks for filter - ([83823b0](https://github.com/stack-rs/mitosis/commit/83823b03df9ab87cb8ff715dcd613cb9a354106d))
- *(schema)* Add task-relavant data types - ([54b723b](https://github.com/stack-rs/mitosis/commit/54b723b20b6d5d46e1d502645890aa5d84752f95))
- *(schema)* Update data types of task - ([c097d95](https://github.com/stack-rs/mitosis/commit/c097d9508bf6e3b7367fb5651ff37b1ad6554e46))
- *(schema)* Move resource management to group - ([7c391bf](https://github.com/stack-rs/mitosis/commit/7c391bf50755315be57d21ee864df8b8706e743f))
- *(schema)* Add uuid of entity task - ([1290839](https://github.com/stack-rs/mitosis/commit/1290839e339b8da334db7fc4201acd57aa15e255))
- *(schema)* Add database entity and migration - ([bbc8016](https://github.com/stack-rs/mitosis/commit/bbc801628fa64d4b2362edbb70a675eb26213881))
- *(service)* Add user login service - ([2efc5bd](https://github.com/stack-rs/mitosis/commit/2efc5bdbcc5d552fda85684e3011761ec979fc41))
- *(service)* Add user management service - ([0da1285](https://github.com/stack-rs/mitosis/commit/0da1285ddae220f5bb51a55458633482ef53d310))
- *(service)* Add s3 service - ([838241e](https://github.com/stack-rs/mitosis/commit/838241e273d0d68634794ce343d35c61557f8f55))
- *(service)* Add auth token service - ([7592eb5](https://github.com/stack-rs/mitosis/commit/7592eb51f0c11a207c3651a5dc161481fb2a311b))
- *(setup)* Make file logger more flexible - ([1098bf2](https://github.com/stack-rs/mitosis/commit/1098bf21def027030339697586eccf7f43eac067))
- *(signal)* Add hook for terminate signals - ([8e42b79](https://github.com/stack-rs/mitosis/commit/8e42b7948f23e81786a83de4a058f9c5d7074c40))
- *(task)* Allow user upload artifact to task - ([a8d2931](https://github.com/stack-rs/mitosis/commit/a8d29313e5b1331b2b9c591fa9809f8a61e90566))
- *(task)* Default only list tasks of the caller - ([42736b0](https://github.com/stack-rs/mitosis/commit/42736b0f82677aebeac81b104569cb12b35c80e7))
- *(task)* Add filter-based tasks query - ([1d0d192](https://github.com/stack-rs/mitosis/commit/1d0d192cad35848d0219e0b27199bcecc3b19845))
- *(task)* Record task state transition in redis - ([6adbfed](https://github.com/stack-rs/mitosis/commit/6adbfed11e90548f5325d3f7499b8f594e666d7a))
- *(task)* Add resource download interfaces - ([c968ec4](https://github.com/stack-rs/mitosis/commit/c968ec4bd17c0e8601a9f112e60cb5fb8f47e130))
- *(task)* Add interface to query task stats - ([7c0b4a6](https://github.com/stack-rs/mitosis/commit/7c0b4a61973b33fbc2cbd027f69c2a3fbcf5e84c))
- *(task)* Add service for managing tasks - ([9e1e3af](https://github.com/stack-rs/mitosis/commit/9e1e3af1b3aa91590f8e9bb52025ec2edd489d36))
- *(user)* Allow user to shutdown workers - ([dae2cad](https://github.com/stack-rs/mitosis/commit/dae2cad692e9c186293509000b4415a79aa7154c))
- *(worker)* Add task timeout check on heartbeat - ([0eb0521](https://github.com/stack-rs/mitosis/commit/0eb05214f997e31073b506b2284acb925b086b10))
- *(worker)* Add option for worker lifetime - ([bb30eab](https://github.com/stack-rs/mitosis/commit/bb30eab7bc198b6a5af8ad93ff344a40c09998ea))
- *(worker)* Relax the constraints of download - ([0c89567](https://github.com/stack-rs/mitosis/commit/0c8956790746f0519976e857951fce3e179611ae))
- *(worker)* Add uuid to env in task execution - ([8c7477c](https://github.com/stack-rs/mitosis/commit/8c7477c8f5da37893ee05e6371351baea03ed088))
- *(worker)* End task state in `Committed` - ([f12204a](https://github.com/stack-rs/mitosis/commit/f12204a1c033e830774ebdd190448c3d0f46fa27))
- *(worker)* Add timeout for download and upload - ([0b442e2](https://github.com/stack-rs/mitosis/commit/0b442e29f694a83f9c7fb688706caf0576d83c78))
- *(worker)* Add service for managing workers - ([1860537](https://github.com/stack-rs/mitosis/commit/186053731523e29fe6120665fbd8bb0e8cffdab8))

### Bug Fixes

- *(s3)* Add ACL for attachments download - ([ad03939](https://github.com/stack-rs/mitosis/commit/ad03939ec6931836292bfdf8e42151f03857372e))
- *(schema)* Change quota from GiB to GB - ([9b49140](https://github.com/stack-rs/mitosis/commit/9b491406dddce471436e2eb01c3f3c3cfcf2017b))
- *(worker)* Revive task after heartbeat timeout - ([c5f2d76](https://github.com/stack-rs/mitosis/commit/c5f2d7620b04834cf33271072a934f72c660d902))
- *(worker)* Resolve bugs of worker not working - ([718ccb0](https://github.com/stack-rs/mitosis/commit/718ccb07957167fdd18c055e6230b6e091c967f9))
- *(worker)* Download resources to right place - ([a430011](https://github.com/stack-rs/mitosis/commit/a4300114cc82128cf440c20ff3c61dd871e21b15))
- *(worker)* Resolve attachment fetch error - ([7ef56c1](https://github.com/stack-rs/mitosis/commit/7ef56c17943b7e4547aba1e113218c5adea7a133))
- *(worker)* Allow empty groups in registration - ([c125411](https://github.com/stack-rs/mitosis/commit/c125411527c76d6a951be6a8046e3e2607ef3ed1))

### Refactor

- *(client)* Decouple redis client - ([89847fe](https://github.com/stack-rs/mitosis/commit/89847fe6d90f7fd239424ae64da27eafae04f1c2))
- *(client)* Extract command handlers - ([83a1003](https://github.com/stack-rs/mitosis/commit/83a100320918f735368c60b93111f0c1c5450cec))
- *(s3)* Check s3 error before updating db - ([01ab340](https://github.com/stack-rs/mitosis/commit/01ab34068f435ea09b246d46f5f5c3d0d2ac32f1))
- *(schema)* Change spec of task - ([a0c955b](https://github.com/stack-rs/mitosis/commit/a0c955b9134f102a7e8105eb3b15ffa3d2c5db8b))
- *(service)* Use array to hold column names - ([450e2c5](https://github.com/stack-rs/mitosis/commit/450e2c5447850040d0824496a5654e0f59bedd37))
- *(task)* Simplify task query logic - ([ecf0489](https://github.com/stack-rs/mitosis/commit/ecf0489b72c1b79ea54b98a37bd6e6085b1a6d71))
- *(worker)* Improve interfaces - ([ca0e9d4](https://github.com/stack-rs/mitosis/commit/ca0e9d4b77b2fa6eeaa928178ee87a60a276b79c))
- Decouple interfaces for flexibility - ([958a1d4](https://github.com/stack-rs/mitosis/commit/958a1d48f79d914ed6a72f57e4843ac0f5580ae3))

### Documentation

- *(client)* Update command line prompts - ([c7b4818](https://github.com/stack-rs/mitosis/commit/c7b48185ffd46c0d24670f3a420d7bbd2be81d14))
- Update some notes - ([4db55da](https://github.com/stack-rs/mitosis/commit/4db55daeefda2992d3ded25786aa2c98b78ead37))
- Add requirements of deps - ([c5342ed](https://github.com/stack-rs/mitosis/commit/c5342ed2fcd9beec30222d18572f32d174fd30a8))
- Add user guide - ([3cc2e6d](https://github.com/stack-rs/mitosis/commit/3cc2e6df9b223b3629b2dd6ea57c94f0c15d026b))

### Styling

- Fix clippy warnings - ([31e8c7f](https://github.com/stack-rs/mitosis/commit/31e8c7f7ec5a472d00b4c77b84d763e940b6064f))

### Miscellaneous Tasks

- *(cliff)* Set values for Github integration - ([e049956](https://github.com/stack-rs/mitosis/commit/e0499567f524cd9cb48070f7d17a50a676687fcd))
- Specify version for netmito in mito - ([4394da2](https://github.com/stack-rs/mitosis/commit/4394da26ad268f965d557c1d61de29327a32c40e))
- Remove extra keywords in manifest - ([748ed1d](https://github.com/stack-rs/mitosis/commit/748ed1d245e4beef0692263a2bfed5bf73ff94ba))
- Add manifest metadata - ([ab301fd](https://github.com/stack-rs/mitosis/commit/ab301fd30645d8ce1b86840866910a4b17643423))
- Update release-plz workflow - ([48a47e9](https://github.com/stack-rs/mitosis/commit/48a47e9d544e1b0a443182c28ae8f1c39d769477))
- Add CHANGELOG - ([7e184fd](https://github.com/stack-rs/mitosis/commit/7e184fdee5b41d66b93729a070ff0514c4f08b50))
- Add release workflows - ([e18f8db](https://github.com/stack-rs/mitosis/commit/e18f8db399f93fe9903c16ce1f1f65a386d01a65))
- Add site-url to mdbook build - ([6e5d053](https://github.com/stack-rs/mitosis/commit/6e5d05341e7fcb3a2acda3b65fc90a225e430edd))
- Add gh-page build workflow - ([a80ff39](https://github.com/stack-rs/mitosis/commit/a80ff394d7c98c16fa47b9bc92b4ee255c7721aa))
- Add deployment configurations - ([4360199](https://github.com/stack-rs/mitosis/commit/4360199053f6d3e4c20c0ff97b07d2eea5b4452a))
- Use clippy directly - ([ca37274](https://github.com/stack-rs/mitosis/commit/ca3727454a9a3ee8176f6b89c3726db457d8e553))
- Update MSRV to 1.76 - ([06980ac](https://github.com/stack-rs/mitosis/commit/06980ac6a3f6762e12326a1a1f57b87fb85fbb3c))
- Initialize project mitosis - ([7d415bf](https://github.com/stack-rs/mitosis/commit/7d415bf05491cec79e0e35a0b1b756a78943b8d2))

## New Contributors

- @BobAnkh made their first contribution

<!-- generated by git-cliff -->
