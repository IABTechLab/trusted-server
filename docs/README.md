# Trusted Server Documentation

VitePress documentation site for Trusted Server.

## Local Development

### Prerequisites

- Node.js (version specified in `.tool-versions`)
- npm

### Setup

```bash
# Install dependencies
npm install

# Start dev server (available at localhost:5173)
npm run dev
```

### Build

```bash
# Build for production
npm run build

# Preview production build
npm run preview
```

## GitHub Pages Deployment

The documentation is automatically deployed to GitHub Pages when changes are pushed to the `main` branch.

### Setup GitHub Pages

1. Go to repository **Settings** → **Pages**
2. Under **Source**, select **GitHub Actions**
3. The workflow in `.github/workflows/deploy-docs.yml` will automatically deploy on push to `main`

### Custom Domain Setup

1. **Update CNAME file**: Edit `docs/public/CNAME` with your domain:

   ```
   docs.yourdomain.com
   ```

2. **Configure DNS**: Add DNS records at your domain provider:

   **Option A - CNAME Record** (recommended for subdomains):

   ```
   Type: CNAME
   Name: docs
   Value: iabtechlab.github.io
   ```

   **Option B - A Records** (for apex domains):

   ```
   Type: A
   Name: @
   Value: 185.199.108.153
   Value: 185.199.109.153
   Value: 185.199.110.153
   Value: 185.199.111.153
   ```

3. **Verify in GitHub**:
   - Go to **Settings** → **Pages**
   - Enter your custom domain
   - Wait for DNS check to pass
   - Enable "Enforce HTTPS"

### Workflow Details

**Trigger**:

- Push to `main` branch (only when `docs/**` changes)
- Manual trigger via Actions tab

**Build Process**:

1. Checkout repository with full history (for `lastUpdated` feature)
2. Setup Node.js (version from `.tool-versions`)
3. Install dependencies (`npm ci`)
4. Build VitePress site (`npm run build`)
5. Upload build artifact
6. Deploy to GitHub Pages

**Permissions Required**:

- `contents: read` - Read repository
- `pages: write` - Deploy to Pages
- `id-token: write` - OIDC token for deployment

## Troubleshooting

### Build Fails in GitHub Actions

**Check**:

- Node.js version matches `.tool-versions`
- All dependencies in `package.json` are correct
- Build succeeds locally (`npm run build`)

**View Logs**:

1. Go to **Actions** tab in GitHub
2. Click on failed workflow run
3. Review build logs

### Custom Domain Not Working

**Check**:

- DNS records propagated (use `dig docs.yourdomain.com`)
- CNAME file exists in `docs/public/CNAME`
- Custom domain verified in GitHub Pages settings
- HTTPS enforced (may take up to 24 hours)

**DNS Verification**:

```bash
# Check CNAME record
dig docs.yourdomain.com CNAME

# Check A records (for apex domain)
dig yourdomain.com A
```

### 404 Errors

**Check**:

- VitePress `base` config (should not be set for custom domains)
- Links use correct paths (start with `/`)
- Build output in `docs/.vitepress/dist` is correct

## Project Structure

```
docs/
├── .vitepress/
│   ├── config.mts          # VitePress configuration
│   └── dist/               # Build output (gitignored)
├── guide/                  # Documentation pages
│   ├── getting-started.md
│   ├── configuration.md
│   └── ...
├── public/                 # Static assets
│   └── CNAME              # Custom domain file
├── index.md               # Homepage
├── package.json           # Dependencies
└── README.md             # This file
```

## Contributing

When adding new documentation:

1. Create `.md` files in `docs/guide/`
2. Update sidebar in `docs/.vitepress/config.mts`
3. Test locally with `npm run dev`
4. Build and verify with `npm run build && npm run preview`
5. Commit and push to trigger deployment

## Links

- **Production**: (Configure your custom domain)
- **GitHub Repo**: https://github.com/IABTechLab/trusted-server
- **VitePress Docs**: https://vitepress.dev
