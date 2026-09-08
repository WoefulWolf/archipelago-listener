import net from "net";
import { Client, ArgumentError } from "archipelago.js";

const TARGET_HOST = process.env.TARGET_HOST || "localhost";
const START_PORT = parseInt(process.env.START_PORT || "50000", 10);
const END_PORT = parseInt(process.env.END_PORT || "50009", 10);
const SCAN_INTERVAL_MS = parseInt(process.env.SCAN_INTERVAL_MS || "30000", 10);
const BOT_NAME = process.env.BOT_NAME || "Archie";
const SLOTS = process.env.SLOTS ? process.env.SLOTS.split(',') : null;
const WEBHOOK_URL = process.env.WEBHOOK_URL || "";

if (!WEBHOOK_URL) {
    console.warn("WARNING: AP_WEBHOOK_URL is not set.");
}

const checkRemoteContainerPort = (port) => {
    return new Promise((resolve) => {
        const socket = new net.Socket();
        socket.setTimeout(250);

        socket.on("connect", () => {
            socket.destroy();
            resolve(true);
        });

        socket.on("error", () => resolve(false));
        socket.on("timeout", () => {
            socket.destroy();
            resolve(false);
        });

        socket.connect(port, TARGET_HOST);
    });
};

function getHexColorFromIndex(index, len) {
    const totalSlots = len <= 0 ? 1 : len;
    const hue = ((index / totalSlots) * 360) % 360;

    const s = 0.85;
    const l = 0.55;

    const c = (1 - Math.abs(2 * l - 1)) * s;
    const x = c * (1 - Math.abs(((hue / 60) % 2) - 1));
    const m = l - c / 2;

    let r = 0, g = 0, b = 0;

    if (0 <= hue && hue < 60) { r = c; g = x; b = 0; }
    else if (60 <= hue && hue < 120) { r = x; g = c; b = 0; }
    else if (120 <= hue && hue < 180) { r = 0; g = c; b = x; }
    else if (180 <= hue && hue < 240) { r = 0; g = x; b = c; }
    else if (240 <= hue && hue < 300) { r = x; g = 0; b = c; }
    else if (300 <= hue && hue <= 360) { r = c; g = 0; b = x; }

    const rInt = Math.round((r + m) * 255);
    const gInt = Math.round((g + m) * 255);
    const bInt = Math.round((b + m) * 255);

    return (rInt << 16) | (gInt << 8) | bInt;
}

class RoomMonitor {
    constructor(port) {
        this.port = port;
        this.client = null;
        this.dataPackage = null;
        this.itemCache = new Map();
        this.locationCache = new Map();
        this.isConnected = false;
        this.isConnecting = false;
    }

    async tick() {
        const isOpen = await checkRemoteContainerPort(this.port);

        if (isOpen && !this.isConnected && !this.isConnecting) {
            console.log(`[Port ${this.port}] Server detected online in container! Connecting...`);
            this.connectToArchipelago();
        }
        else if (!isOpen && this.isConnected) {
            console.log(`[Port ${this.port}] Server went offline inside container. Cleaning up.`);
            this.disconnect();
        }
    }

    async buildLookupCaches() {
        this.dataPackage = await this.client.package.fetchPackage();

        this.itemCache.clear();
        this.locationCache.clear();

        if (!this.dataPackage?.games) return;

        for (const [gameName, gamePkg] of Object.entries(this.dataPackage.games)) {
            if (gamePkg.item_name_to_id) {
                for (const [itemName, id] of Object.entries(gamePkg.item_name_to_id)) {
                    this.itemCache.set(`${gameName}:${id}`, itemName);
                }
            }

            if (gamePkg.location_name_to_id) {
                for (const [locName, id] of Object.entries(gamePkg.location_name_to_id)) {
                    this.locationCache.set(`${gameName}:${id}`, locName);
                }
            }
        }
    }

    async connectToArchipelago() {
        if (!SLOTS) return;

        this.isConnecting = true;
        this.client = new Client();

        try {
            const wsUrl = `ws://${TARGET_HOST}:${this.port}`;
            console.log(`[Port ${this.port}] Initializing Read-Only Global Connection to ${wsUrl}...`);

            let connected = false;

            for (const slot of SLOTS) {
                try {
                    await this.client.login(
                        wsUrl,
                        `${slot}`,
                        "",
                        {
                            slotData: false,
                            tags: ["Tracker"]
                        }
                    );
                    connected = true;
                    break;
                } catch (err) {
                    console.warn(`[Port ${this.port}] Slot "${slot}" failed: ${err.message || err}`);
                }
            }

            if (!connected) {
                throw new Error("All slots failed to authenticate.");
            }

            this.isConnected = true;
            this.isConnecting = false;
            console.log(`[Port ${this.port}] Connected.`);
            await this.buildLookupCaches();

            this.client.socket.on("printJSON", (packet) => {
                if (packet.type === "ItemSend" || packet.type === "Hint" || packet.receiving !== undefined) {
                    this.onLocationChecked(packet);
                }
            });

        } catch (err) {
            console.error(`[Port ${this.port}] Handshake Failed: ${err.message || err}`);
            this.isConnected = false;
            this.isConnecting = false;
            this.client = null;
        }
    }

    disconnect() {
        this.isConnected = false;
        this.isConnecting = false;
        if (this.client) {
            try { this.client.disconnect(); } catch (e) { }
            this.client = null;
        }
    }

    getPlayer(id) {
        return this.client.players.findPlayer(id) || null;
    }

    getItemName(game, itemId) {
        return this.itemCache.get(`${game}:${itemId}`) || null;
    }

    getLocationName(game, locationId) {
        return this.locationCache.get(`${game}:${locationId}`) || null;
    }

    async onLocationChecked(packet) {
        if (!WEBHOOK_URL) return;

        const targetPlayerId = packet.receiving ?? packet.item?.player;
        const targetGame = this.getPlayer(targetPlayerId)?.game || "";

        const formattedMessage = packet.data.map(piece => {
            switch (piece.type) {
                case "player_id": {
                    const id = parseInt(piece.text, 10);
                    const player = this.getPlayer(id);
                    return `**${player?.alias || player?.name || piece.text}**`;
                }

                case "item_id": {
                    const game = this.getPlayer(piece.player)?.game || "";
                    const id = parseInt(piece.text, 10);
                    const name = this.getItemName(game, id);
                    return `__${name || piece.text}__`;
                }

                case "location_id": {
                    const game = this.getPlayer(piece.player)?.game || "";
                    const id = parseInt(piece.text, 10);
                    const name = this.getLocationName(game, id);
                    return `*${name || piece.text}*`;
                }

                case "player_name":
                    return `**${piece.text}**`;

                case "item_name":
                    return `__${piece.text}__`;

                case "location_name":
                    return `*${piece.text}*`;

                default:
                    return piece.text;
            }
        }).join("");

        const embedColor = getHexColorFromIndex(this.port - START_PORT, END_PORT - START_PORT);

        try {
            const response = await fetch(WEBHOOK_URL, {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({
                    username: BOT_NAME,
                    embeds: [
                        {
                            title: "",
                            description: formattedMessage,
                            color: embedColor,
                            fields: []
                        }
                    ]

                })
            });

            if (!response.ok) {
                const errorText = await response.text();
                console.error(`[Port ${this.port}] Webhook failed with status ${response.status}:`, errorText);
            }
        } catch (err) {
            console.error(`[Port ${this.port}] Failed to send webhook: `, err.message);
        }
    }
}

console.log(`Starting archipelago- listener...`);
console.log(`Target Range: ${TARGET_HOST}: ${START_PORT} ➔ ${TARGET_HOST}: ${END_PORT}`);

const monitors = [];
for (let port = START_PORT; port <= END_PORT; port++) {
    monitors.push(new RoomMonitor(port));
}

setInterval(async () => {
    await Promise.all(monitors.map(monitor => monitor.tick()));
}, SCAN_INTERVAL_MS);
