const unsignedMac = process.env.SMIDR_UNSIGNED_MAC_BUILD === '1';
const hasAppleNotaryCredentials = Boolean(
  (process.env.APPLE_API_KEY && process.env.APPLE_API_KEY_ID && process.env.APPLE_API_ISSUER) ||
  (process.env.APPLE_ID && process.env.APPLE_APP_SPECIFIC_PASSWORD && process.env.APPLE_TEAM_ID) ||
  (process.env.APPLE_KEYCHAIN && process.env.APPLE_KEYCHAIN_PROFILE)
);

module.exports = {
  appId: 'com.smidr.desktop',
  productName: 'Smiðr',
  executableName: 'smidr-desktop',
  artifactName: 'Smidr-${version}-${arch}.${ext}',
  asar: true,
  npmRebuild: false,
  directories: {
    output: 'dist'
  },
  files: [
    'main.cjs',
    'lib/**/*',
    'package.json'
  ],
  extraResources: [
    {
      from: 'generated/bin',
      to: 'bin'
    },
    {
      from: 'generated/runtime',
      to: 'runtime'
    }
  ],
  linux: {
    syncDesktopName: true,
    target: ['AppImage'],
    category: 'Graphics',
    icon: 'build/icon.png',
    desktop: {
      entry: {
        Name: 'Smiðr',
        Comment: 'AI-assisted parametric 3D modeling',
        Keywords: 'CAD;3D;CadQuery;modeling;',
        StartupWMClass: 'smidr-desktop'
      }
    }
  },
  mac: {
    target: ['dmg', 'zip'],
    category: 'public.app-category.graphics-design',
    icon: 'build/icon.svg',
    minimumSystemVersion: '12.0',
    hardenedRuntime: !unsignedMac,
    notarize: !unsignedMac && hasAppleNotaryCredentials,
    entitlements: 'build/entitlements.mac.plist',
    entitlementsInherit: 'build/entitlements.mac.inherit.plist',
    ...(unsignedMac ? { identity: null } : {})
  },
  dmg: {
    title: 'Smiðr ${version}'
  }
};
