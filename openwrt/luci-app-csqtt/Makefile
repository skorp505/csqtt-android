include $(TOPDIR)/rules.mk

PKG_NAME:=luci-app-csqtt
PKG_VERSION:=2.1.13
PKG_RELEASE:=4

LUCI_TITLE:=LuCI web panel for the CSQTT OpenWrt client
LUCI_DESCRIPTION:=Web panel to configure the CSQTT client (TUN or SOCKS5) running on the router.
LUCI_DEPENDS:=+luci-base +csqtt-client
LUCI_PKGARCH:=all

PKG_LICENSE:=PolyForm-Noncommercial-1.0.0
PKG_MAINTAINER:=skorp505 <skorp505@gmail.com>

include $(TOPDIR)/feeds/luci/luci.mk

# call BuildPackage - OpenWrt buildroot signature