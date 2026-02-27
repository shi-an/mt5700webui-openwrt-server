include $(TOPDIR)/rules.mk

PKG_NAME:=at-webserver
PKG_VERSION:=0.1.0
PKG_RELEASE:=1

PKG_BUILD_DEPENDS:=rust/host
PKG_BUILD_DIR:=$(BUILD_DIR)/$(PKG_NAME)-$(PKG_VERSION)
PKG_BUILD_PARALLEL:=1

include $(INCLUDE_DIR)/package.mk
include $(TOPDIR)/feeds/packages/lang/rust/rust-package.mk

define Package/at-webserver
  SECTION:=net
  CATEGORY:=Network
  TITLE:=AT Command WebSocket Server (Rust)
  # 必须引入 ca-bundle，否则使用 rustls 请求外部 https 时会因为找不到根证书报错
  DEPENDS:=$(RUST_ARCH_DEPENDS) +ca-bundle
endef

define Package/at-webserver/description
  Rust implementation of AT Webserver for OpenWrt.
  Provides WebSocket interface for AT commands.
endef

define Build/Prepare
	mkdir -p $(PKG_BUILD_DIR)
	$(CP) ./src $(PKG_BUILD_DIR)/
	$(CP) ./files $(PKG_BUILD_DIR)/
	$(CP) ./Cargo.toml $(PKG_BUILD_DIR)/
	if [ -f ./Cargo.lock ]; then $(CP) ./Cargo.lock $(PKG_BUILD_DIR)/; fi
endef

define Package/at-webserver/install
	$(INSTALL_DIR) $(1)/usr/bin
	$(INSTALL_BIN) $(PKG_INSTALL_DIR)/bin/at-webserver $(1)/usr/bin/
	
	$(INSTALL_DIR) $(1)/etc/init.d
	$(INSTALL_BIN) ./files/etc/init.d/at-webserver $(1)/etc/init.d/

	$(INSTALL_DIR) $(1)/etc/config
	$(INSTALL_CONF) ./files/etc/config/at-webserver $(1)/etc/config/

	$(INSTALL_DIR) $(1)/www/5700
	$(CP) -r ./files/www/5700/* $(1)/www/5700/
	
	$(INSTALL_DIR) $(1)/www/cgi-bin
	$(INSTALL_BIN) ./files/www/cgi-bin/at-ws-info $(1)/www/cgi-bin/
	$(INSTALL_BIN) ./files/www/cgi-bin/at-log-clear $(1)/www/cgi-bin/
endef

$(eval $(call RustBinPackage,at-webserver))
$(eval $(call BuildPackage,at-webserver))
