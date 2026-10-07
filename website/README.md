# Public website

Canonical URL: https://sigmadock.dev. The static source is included here for review and reuse. Preview/public hosting URL: https://sigmadock.celestia-lab-1897.chatgpt.site.

The editable hosting checkout is `/Users/sysrex/Work/sigmadock-site`, with `dist/` as the static output. Copy changes from this directory into that checkout, then use the Sites source workflow to commit, package and publish the matching source revision. Hosting credentials never belong in the repository. This site has no external fonts, tracking, analytics or runtime JavaScript.

## Domain activation

The domain is owned by the project and uses Namecheap DNS. The hosting provider requires the following records (Namecheap host labels shown); use Automatic TTL and remove conflicting parking records for `@`:

| Type | Host | Value |
| --- | --- | --- |
| A | @ | 162.159.143.30 |
| A | @ | 172.66.3.26 |
| TXT | _openai-site-verification | openai-site-verification=CLJvYJVYGxP7qdTVcQ-sAqx4GpMGS4B92B6So8E0b8M |
| TXT | _cf-custom-hostname | 1e5df65f-2985-4f83-aabb-ceb951aed435 |

Verification and HTTPS certificate issuance are pending those records. These public ownership verification values are DNS records, not application credentials. Recheck the hosting provider if the custom domain is removed and recreated. No redirects on sigmauno.com have been configured: that site's hosting configuration is not available in this checkout.
