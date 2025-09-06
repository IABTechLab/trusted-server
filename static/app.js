/* EdgePubs Static JavaScript - Cacheable */

// Ad loading functions - keep fresh, don't cache results
async function loadServerSideAd() {
    const adSlot = document.getElementById('server-side-ad-slot');
    if (!adSlot) return;
    
    adSlot.innerHTML = '<span>Loading server-side ad (Prebid → GAM)...</span>';
    
    try {
        // Add cache busting to ad requests only  
        const timestamp = Date.now();
        const response = await fetch(`/server-side-ad?cb=${timestamp}`, {
            method: 'GET',
            headers: {
                'X-Consent-Advertising': 'true',
                'X-Ad-Slot': 'header-728x90',
                'Cache-Control': 'no-cache'
            }
        });
        
        const data = await response.json();
        console.log('Server-side ad response:', data);
        console.log('Response status:', data.status);
        console.log('Ad HTML length:', data.ad_slot_html ? data.ad_slot_html.length : 0);
        
        if (data.status === 'server_side_ad_success' && data.ad_slot_html) {
            console.log('Setting innerHTML for ad slot with content length:', data.ad_slot_html.length);
            console.log('First 200 chars of creative:', data.ad_slot_html.substring(0, 200));
            adSlot.innerHTML = data.ad_slot_html;
            console.log('Server-side ad loaded successfully');
            console.log('Ad slot innerHTML after setting:', adSlot.innerHTML.substring(0, 200));
        } else {
            console.log('No successful bid, showing fallback');
            adSlot.innerHTML = data.ad_slot_html || '<span style="color: #999;">No ad available</span>';
        }
    } catch (error) {
        console.error('Server-side ad loading failed:', error);
        adSlot.innerHTML = '<span style="color: #999;">Ad temporarily unavailable</span>';
    }
}

function loadGAMScript() {
    console.log('Loading GAM script from edgepubs.com domain');
    const script = document.createElement('script');
    script.src = '/tag/js/test.js';
    script.async = true;
    script.onload = function() {
        console.log('GAM script loaded, initializing ads');
        initializeGAMAds();
    };
    script.onerror = function() {
        console.error('Failed to load GAM script');
    };
    document.head.appendChild(script);
}

function initializeGAMAds() {
    // GAM initialization would go here
    console.log('GAM ads initialized');
}

// GDPR consent management
function showGDPRBanner() {
    const banner = document.getElementById('gdpr-banner');
    if (banner) {
        banner.classList.add('visible');
    }
}

function hideGDPRBanner() {
    const banner = document.getElementById('gdpr-banner');
    if (banner) {
        banner.classList.remove('visible');
    }
}

// Initialize page when DOM loads
document.addEventListener('DOMContentLoaded', function() {
    // Check for existing consent
    const consentData = getConsentFromCookie();
    
    if (!consentData) {
        showGDPRBanner();
    } else {
        if (consentData.advertising) {
            loadServerSideAd();
            loadGAMScript();
        }
    }
});

function getConsentFromCookie() {
    // Simple cookie parsing - this would integrate with your GDPR implementation
    const cookies = document.cookie.split(';');
    for (let cookie of cookies) {
        if (cookie.trim().startsWith('gdpr_consent=')) {
            try {
                return JSON.parse(decodeURIComponent(cookie.split('=')[1]));
            } catch (e) {
                console.error('Error parsing consent cookie:', e);
            }
        }
    }
    return null;
}