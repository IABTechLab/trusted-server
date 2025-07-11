# Trusted Server 

:information_source: Trusted Server is an open-source runtime and edge-orchestration layer for modern publishers - executing 3rd-party scripts and your entire ad-stack server-side, all under 1st-party control. Our goal is to move ad-related code execution and control from web browsers to publisher controlled, "trusted" edge-cloud infrastructure. 

Trusted Server is the new execution layer for the open-web - returning control of 1st party data, security, and overall user-experience back to publishers.

At this time, Trusted Server is designed to work with Fastly Compute. Follow these steps to configure Fastly Compute and deploy it.

## Fastly
- Create account at Fastly if you don’t have one - manage.fastly.com
- Log in to the Fastly control panel. 
    - Go to Account > API tokens > Personal tokens. 
    - Click Create token
    - Name the Token
    - Choose User Token
    - Choose Global API Access
    - Choose what makes sense for your Org in terms of Service Access
    - Copy key to a secure location because you will not be able to see it again

- Create new Compute Service 
    - Click Compute and Create Service 
    - Click “Create Empty Service” (below main options) 
    - Add your domain of the website you’ll be testing or using and click update
    - Click on “Origins” section and add your ad-server / ssp partner information as hostnames (note after you save this information you can select port numbers and TLS on/off) 
    - IMPORTANT: when you enter the FQDN or IP ADDR information and click Add you need to enter a “Name” in the first field that will be referenced in your code so something like “my_ad_partner_1” 
    - 

:warning: Fastly gives you a test domain to play on but obviously you’re free to create a CNAME to your domain when you’re ready. Note that Fastly compute ONLY accepts client traffic from TLS 

## Installation

### Brew

:warning: Follow the prompts before and afterwards (to configure system path, etc)

#### Install Brew

```sh
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
```
### Fastly CLI

#### Install Fastly CLI 
```sh
brew install fastly/tap/fastly
```

#### Verify Installation and Version 
```sh
fastly version
```

:warning: fastly cli version should be at least v11.3.0

#### Create profile and follow interactive prompt for pasting your API Token created earlier:
```sh	
fastly profile create
```

### Rust

#### Install Rust with asdf
```sh
brew install asdf
asdf plugin add rust
asdf install rust $(grep '^rust ' .tool-versions | awk '{print $2}')
asdf reshim
```

#### Fix path for Bash

Edit ~/.bash_profile to add path for asdf shims: 
```sh
export PATH="${ASDF_DATA_DIR:-$HOME/.asdf}/shims:$PATH"
```

#### Fix path for ZSH

Edit ~/.zshrc to add path for asdf shims: 
```sh
export PATH="${ASDF_DATA_DIR:-$HOME/.asdf}/shims:$PATH"
```

#### Other shells
See https://asdf-vm.com/guide/getting-started.html#_2-configure-asdf


### Configure Build and

#### Clone Project
```sh
git clone git@github.com:IABTechLab/trusted-server.git
```

### Configure
#### Edit configuration files
:information_source: Note that you’ll have to edit the following files for your setup:

- fastly.toml (service ID, author, description) 
- trusted-server.toml (KV store ID names) 

### Build

```sh
cargo build
```

### Deploy to Fastly

```sh
fastly compute publish
```

## Devleopment

#### Install viceroy for running tests
```sh
cargo install viceroy
```

#### Run Fastly server locally
- Review configuration for [local_server](fastly.toml#L16)
- Review env variables overrides in [.env.dev](.env.dev)

```sh
export $(grep -v '^#' .env.dev | xargs -0)
```

```sh
fastly -i compute serve
```

#### Tests
```sh
cargo test
```

:warning: if test fails `viceroy` will not display line number of the failed test. Rerun it with `cargo test_details`.

#### Additional Rust Commands
- `cargo fmt`: Ensure uniform code formatting
- `cargo clippy`: Ensure idiomatic code
- `cargo check`: Ensure compilation succeeds on Linux, MacOS, Windows and WebAssembly
- `cargo bench`: Run all benchmarks
