Steps to install Trusted Server 

FASTLY COMPUTE SETUP on Mac OS 

- Create account at Fastly if you don’t have one - manage.fastly.com

- Log in to the Fastly control panel. Go to Account > API tokens > Personal tokens. 
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

NOTE: Fastly gives you a test domain to play on but obviously you’re free to create a CNAME to your domain when you’re ready. Note that Fastly compute ONLY accepts client traffic from TLS   	- 
- Install Brew if you don’t have it. Open terminal and paste the following and follow the prompts before and afterwards (to configure system path, etc):   /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

- Install Fastly CLI 
	
	brew install fastly/tap/fastly

- Verify Installation and Version 
	
	fastly version 

- Create profile and follow interactive prompt for pasting your API Token created earlier:
	
	fastly profile create

INSTALL RUST using asdf on Mac 

	brew install asdf
	asdf plugin add rust
	asdf install rust 1.83.0
	asdf reshim

- Edit .zshrc profile to add path for asdf shims: 

	export PATH="${ASDF_DATA_DIR:-$HOME/.asdf}/shims:$PATH"


CREATE FASTLY COMPUTE SERVICE 

- Follow prompts with information and choose Rust and an empty starter kit option
	Fastly compute init


CLONE PROJECT from GH

git clone https://<username>@github.com/IABTechLab/trusted-server.git 
- This will give you important files such as .tool-versions that you’ll need to run some commands 
- Note that you’ll have to edit the following files for your setup: 
    - fastly.toml (service ID, author, description) 
    - Potsi.toml (KV store ID names) 




FASTLY SERVICE CONFIGURATION

- Begin in the UI at manage.fastly.com 
- Click Compute -> Create Service
- 

