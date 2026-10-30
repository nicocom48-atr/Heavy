# ⚡ HeavyDrop: Transferencia P2P Masiva (50GB+) mediante Códigos Temporales

**HeavyDrop** es una herramienta en Rust diseñada para la transferencia punto a punto (P2P) de archivos de cualquier tamaño (50 GB, 100 GB, 1 TB o más) de forma rápida, privada y sin complicaciones de red. 

Dos usuarios pueden conectarse y transferir archivos compartiendo **únicamente un código temporal único**, sin necesidad de conocer, copiar ni configurar direcciones IP, puertos o comandos de red.

> **Nota de Distribución Oficial:** Las versiones oficiales precompiladas de HeavyDrop se distribuyen **exclusivamente para Windows x64**.

---

## 🚀 Experiencia de Usuario Simplificada

### 📤 Emisor
1. Abre HeavyDrop.
2. Selecciona o arrastra el archivo (hasta 1 TB+).
3. Pulsa **"Crear transferencia"**.
4. La aplicación genera un código único de 12 caracteres (ejemplo: `KD7M-9Q2P-W8RX`) con validez de 5 minutos.
5. Comparte ese código con el receptor.

### 📥 Receptor
1. Abre HeavyDrop.
2. Introduce el código en la pestaña **"Recibir"**.
3. Pulsa **"Usar código y conectar"**.
4. El archivo comienza a descargarse automáticamente con verificación en tiempo real.

---

## 🌐 Conectividad Inteligente en 3 Vías

El motor de HeavyDrop resuelve automáticamente la mejor ruta de conexión disponible de forma transparente para el usuario:

```text
               [ Emisor HeavyDrop ] 
                        │
       ┌────────────────┼────────────────┐
       ▼                ▼                ▼
[ 1. Directa LAN ]   [ 2. Directa WAN ]   [ 3. Relay Cifrado E2E ]
Beacon UDP Ciego     Candidatos Públicos    Fallback para CGNAT/Firewalls
Local en ms          P2P por Internet       100% Cifrado E2E (Dumb Pipe)
       ▲                ▲                ▲
       └────────────────┼────────────────┘
                        │
              [ Receptor HeavyDrop ]
```

1. **Conexión Directa Local (LAN):** Detección instantánea en la misma red local mediante baliza UDP con ticket ciego. No requiere acceso a Internet ni expone datos a otros equipos de la red Wi-Fi.
2. **Conexión Directa por Internet (P2P):** Intercambio de candidatos de red a través de un servidor ligero de señalización (*rendezvous*).
3. **Fallback mediante Relay Cifrado:** Si ambos equipos se encuentran detrás de firewalls restrictivos o CGNAT simétrico, la transferencia se canaliza a través del servidor como un túnel ciego. **Todo el contenido viaja cifrado de extremo a extremo con ChaCha20-Poly1305**, de modo que el relay jamás tiene acceso a las claves ni al contenido del archivo.

---

## 🔒 Arquitectura y Seguridad Criptográfica

1. **Códigos de Emparejamiento Seguros y Legibles:**
   - 12 caracteres en Base32 Crockford (`XXXX-XXXX-XXXX`), excluyendo caracteres ambiguos (`I`, `L`, `O`, `U`).
   - 60 bits de entropía real generados mediante entropía criptográfica del sistema operativo (`getrandom`).
   - Caducidad automática a los 5 minutos e invalidación de un solo uso para prevenir ataques de repetición (*replay attacks*).
2. **Autenticación Mutua de Conocimiento Cero (Zero-Disclosure):**
   - El código nunca se envía en texto claro ni por la red local ni al servidor de señalización.
   - Ambas partes generan y verifican pruebas criptográficas independientes con BLAKE3 en tiempo constante (`constant_time_eq`), impidiendo ataques de intermediario (*Man-In-The-Middle*) o servidores de señalización maliciosos.
3. **Cifrado de Transporte AEAD (ChaCha20-Poly1305 + X25519):**
   - Intercambio de claves efímero **X25519** (*Perfect Forward Secrecy*).
   - Aislamiento de nonces por dirección y salt de sesión único.
   - Cada bloque de 4 MB se autentica con Poly1305 y hash BLAKE3 antes de guardarse en disco.
4. **Reanudación Verificable con Archivos `.part`:**
   - Si se corta la conexión a los 40 GB de un archivo de 50 GB, el receptor verifica el último fragmento local guardado y reanuda exactamente desde ese punto. Si hubo corrupción previa en disco, descarta el tramo inválido automáticamente.
   - Renombrado atómico al completar el 100% y verificar la integridad global.
5. **Consumo de Memoria Acotado (< 20 MB RAM):**
   - Streaming continuo por fragmentos de 4 MB. Nunca se carga el archivo completo en memoria.
6. **Protección Estricta contra Path Traversal:**
   - Bloqueo de rutas relativas (`../`) y nombres reservados del sistema Windows (`CON`, `PRN`, `AUX`, `NUL`, etc.).

---

## 🖥️ Interfaz Gráfica Nativa (GUI)

Al abrir `heavydrop.exe` sin argumentos (o haciendo doble clic), se inicia la aplicación de escritorio simplificada:

- 📤 **Enviar:** Arrastra cualquier archivo, pulsa "Crear transferencia", copia el código con 1 clic y observa la cuenta regresiva.
- 📥 **Recibir:** Ingresa el código recibido, selecciona la carpeta de destino y descarga con un solo clic.
- 📊 **Monitor:** Velocidad en tiempo real (MB/s), bytes transferidos, porcentaje, tiempo restante (ETA) y etiqueta de conectividad (*Directa LAN*, *Directa Internet* o *Relay*).

---

## 📖 Uso por Línea de Comandos (CLI)

### 1. Enviar un archivo (genera el código automáticamente)

```bash
heavydrop send "F:\backups\archivo_50gb.iso"
```

Salida:
```text
================ [ HEAVYDROP P2P TRANSFER ] ================
  Archivo : archivo_50gb.iso (50.00 GB)
  Código  : KD7M-9Q2P-W8RX
  Estado  : Esperando que el receptor ingrese el código (04:59)
============================================================
```

### 2. Recibir un archivo usando el código

```bash
heavydrop recv KD7M-9Q2P-W8RX --output-dir "D:\Descargas"
```

El receptor buscará al emisor por red local y señalización de Internet automáticamente.

### 3. Servidor de Señalización y Relay (necesario entre redes distintas)

**No hay un servidor público configurado por defecto.** Sin configurar uno, el código
solo permite descubrir al emisor dentro de la misma red local. Para conectar dos
equipos en redes distintas, ambos deben utilizar el mismo servidor accesible por
Internet. El código identifica la transferencia; no contiene una dirección de red.

Si deseas alojar tu propio servicio de señalización y relay en un servidor o VPS:

```bash
heavydrop server --port 7880
```

Puedes configurar a los clientes para usar tu servidor mediante la variable de entorno:
```powershell
$env:HEAVYDROP_SIGNAL_SERVER = "tu-servidor.com:7880"
```

En la interfaz gráfica también puedes completar **Servidor de Internet** con
`tu-servidor.com:7880` en ambos equipos antes de iniciar la transferencia. La
variable de entorno se lee al abrir la aplicación. El campo de la ventana se
aplica a esa ejecución y no se guarda al cerrarla.

El servidor debe aceptar TCP entrante en el puerto 7880 y ambos clientes deben
poder conectarse a él. Si la conexión directa está bloqueada por NAT/CGNAT,
los clientes usan el relay del mismo servidor con el mismo cifrado de extremo a
extremo. El intercambio de candidatos por sí solo no abre puertos en el router.
En una LAN, permite HeavyDrop en el firewall de Windows (TCP de transferencia y
UDP 7879 para descubrimiento).

---

## 📦 Descargas Oficiales (Windows x64)

Los binarios oficiales optimizados se publican automáticamente en la sección de **[Releases en GitHub](https://github.com/nicocom48-atr/Heavy/releases)**:

- 🪟 **Windows x64:** `heavydrop-vX.Y.Z-windows-x64.zip` (incluye `heavydrop.exe` optimizado y su checksum SHA-256).

Para comprobar la integridad del archivo descargado en PowerShell:
```powershell
Get-FileHash -Algorithm SHA256 heavydrop-vX.Y.Z-windows-x64.zip
```
