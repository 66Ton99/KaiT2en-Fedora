// SPDX-License-Identifier: GPL-2.0
#include <linux/module.h>
#include <linux/netdevice.h>
#include <linux/rtnetlink.h>
#include <linux/usb.h>
#include <linux/usb/usbnet.h>

static char *ifname = NULL;
module_param(ifname, charp, 0644);
MODULE_PARM_DESC(ifname, "Optional explicit interface name for Apple T2 CDC-NCM device");

/* Recover missing CDC network-connection notification on Apple T2's private
 * point-to-point link. Use usbnet's normal link-change worker, with no reset.
 */
static struct net_device *find_t2_ncm_dev(void)
{
	struct net_device *net = NULL;
	struct net_device *dev;

	if (ifname && ifname[0]) {
		dev = dev_get_by_name(&init_net, ifname);
		if (dev) {
			if (dev->dev.parent && dev->dev.parent->driver &&
			    strcmp(dev->dev.parent->driver->name, "cdc_ncm") == 0) {
				struct usbnet *usbnet = netdev_priv(dev);
				if (le16_to_cpu(usbnet->udev->descriptor.idVendor) == 0x05ac &&
				    le16_to_cpu(usbnet->udev->descriptor.idProduct) == 0x8233) {
					return dev;
				}
			}
			dev_put(dev);
		}
		return NULL;
	}

	for_each_netdev(&init_net, dev) {
		if (!dev->dev.parent || !dev->dev.parent->driver)
			continue;
		if (strcmp(dev->dev.parent->driver->name, "cdc_ncm") != 0)
			continue;
		struct usbnet *usbnet = netdev_priv(dev);
		if (le16_to_cpu(usbnet->udev->descriptor.idVendor) == 0x05ac &&
		    le16_to_cpu(usbnet->udev->descriptor.idProduct) == 0x8233) {
			dev_hold(dev);
			net = dev;
			break;
		}
	}

	return net;
}

static int __init t2_touchid_link_init(void)
{
	struct net_device *net;
	struct usbnet *usbnet;
	int rc = -ENODEV;

	rtnl_lock();
	net = find_t2_ncm_dev();
	if (!net) {
		pr_warn("t2_touchid_link: Apple T2 CDC-NCM device (05ac:8233) not found\n");
		goto out;
	}

	if (!netif_running(net)) {
		rc = dev_open(net, NULL);
		if (rc < 0) {
			pr_warn("t2_touchid_link: failed to open %s: %d\n", net->name, rc);
			goto put;
		}
	}

	usbnet = netdev_priv(net);
	usbnet_link_change(usbnet, true, false);
	pr_info("t2_touchid_link: scheduling link-ready notification on %s (05ac:8233)\n", net->name);
	rc = 0;

put:
	dev_put(net);
out:
	rtnl_unlock();
	return rc;
}

static void __exit t2_touchid_link_exit(void)
{
}

module_init(t2_touchid_link_init);
module_exit(t2_touchid_link_exit);

MODULE_AUTHOR("Ton Sharp <github@66ton99.org.ua>");
MODULE_DESCRIPTION("Recover missing link notification on Apple T2 internal CDC-NCM");
MODULE_LICENSE("GPL");
