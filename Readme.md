# Electrsd

Utility to run a regtest electrsd process connected to a given [bitcoind](https://github.com/RCasatta/bitcoind) instance, 
useful in integration testing environment.

```
let bitcoind = bitcoind::BitcoinD::new("/usr/local/bin/bitcoind").unwrap();
let electrsd = electrsd::ElectrsD::new("/usr/local/bin/electrsd", bitcoind).unwrap();
let header = electrsd.client.block_headers_subscribe().unwrap();
assert_eq!(header.height, 0);
```

## Features

  * electrsd use a temporary directory as db dir
  * A free port is asked to the OS (a very low probability race condition is still possible) 
  * the process is killed when the struct goes out of scope no matter how the test finishes
