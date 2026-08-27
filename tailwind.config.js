/** @type {import('tailwindcss').Config} */
export default {
  darkMode: ['class', '[data-theme="dark"]'],
  content: ['./index.html', './src/**/*.{ts,tsx}'],
  theme: {
  	extend: {
  		colors: {
  			background: 'hsl(var(--background))',
  			foreground: 'hsl(var(--foreground))',
  			card: {
  				DEFAULT: 'hsl(var(--card))',
  				foreground: 'hsl(var(--card-foreground))'
  			},
  			popover: {
  				DEFAULT: 'hsl(var(--popover))',
  				foreground: 'hsl(var(--popover-foreground))'
  			},
  			primary: {
  				DEFAULT: 'hsl(var(--primary))',
  				foreground: 'hsl(var(--primary-foreground))'
  			},
  			secondary: {
  				DEFAULT: 'hsl(var(--secondary))',
  				foreground: 'hsl(var(--secondary-foreground))'
  			},
  			muted: {
  				DEFAULT: 'hsl(var(--muted))',
  				foreground: 'hsl(var(--muted-foreground))'
  			},
  			accent: {
  				DEFAULT: 'hsl(var(--accent))',
  				foreground: 'hsl(var(--accent-foreground))'
  			},
  			destructive: {
  				DEFAULT: 'hsl(var(--destructive))',
  				foreground: 'hsl(var(--destructive-foreground))'
  			},
  			border: 'hsl(var(--border))',
  			input: 'hsl(var(--input))',
  			ring: 'hsl(var(--ring))',
  			surface: 'var(--color-surface)',
  			'on-surface': 'var(--color-on-surface)',
  			'primary-container': 'var(--color-primary-container)',
  			'on-primary-container': 'var(--color-on-primary-container)',
  			'secondary-container': 'var(--color-secondary-container)',
  			'on-secondary-container': 'var(--color-on-secondary-container)',
  			tertiary: 'var(--color-tertiary)',
  			'on-tertiary': 'var(--color-on-tertiary)',
  			'tertiary-container': 'var(--color-tertiary-container)',
  			'on-tertiary-container': 'var(--color-on-tertiary-container)',
  			'error-container': 'var(--color-error-container)',
  			'on-error-container': 'var(--color-on-error-container)',
  			'surface-container': 'var(--color-surface-container)',
  			'surface-container-low': 'var(--color-surface-container-low)',
  			'surface-container-high': 'var(--color-surface-container-high)',
  			'surface-container-highest': 'var(--color-surface-container-highest)',
  			outline: 'var(--color-outline)',
  			'outline-variant': 'var(--color-outline-variant)',
  			inverse: 'var(--color-inverse)',
  			'on-inverse': 'var(--color-on-inverse)',
  			openai: 'var(--color-openai-brand)',
  			anthropic: 'var(--color-anthropic-brand)',
  			sidebar: {
  				DEFAULT: 'hsl(var(--sidebar-background))',
  				foreground: 'hsl(var(--sidebar-foreground))',
  				primary: 'hsl(var(--sidebar-primary))',
  				'primary-foreground': 'hsl(var(--sidebar-primary-foreground))',
  				accent: 'hsl(var(--sidebar-accent))',
  				'accent-foreground': 'hsl(var(--sidebar-accent-foreground))',
  				border: 'hsl(var(--sidebar-border))',
  				ring: 'hsl(var(--sidebar-ring))'
  			}
  		},
  		fontFamily: {
  			sans: [
  				'PingFang SC',
  				'system-ui',
  				'sans-serif'
  			]
  		},
  		borderRadius: {
  			lg: 'var(--radius)',
  			md: 'calc(var(--radius) - 2px)',
  			sm: 'calc(var(--radius) - 4px)'
  		},
  		keyframes: {
  			'accordion-down': {
  				from: {
  					height: '0'
  				},
  				to: {
  					height: 'var(--radix-accordion-content-height)'
  				}
  			},
  			'accordion-up': {
  				from: {
  					height: 'var(--radix-accordion-content-height)'
  				},
  				to: {
  					height: '0'
  				}
  			}
  		},
  		animation: {
  			'accordion-down': 'accordion-down 0.2s ease-out',
  			'accordion-up': 'accordion-up 0.2s ease-out'
  		}
  	}
  },
  plugins: [require('tailwindcss-animate')],
};
