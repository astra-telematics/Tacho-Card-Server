# Device <-> Server Protocol

We need a way for the device and server to communicate and relay tachograph and smart card messages.

## Packet Structure

- 1 byte: '$'
- 2 byte: message type
- 2 byte: message length

Messages can be of several types depending on the message type value

- 0: Ping (device/server)
- 1: Pong (device/server)
- 2: Connection Announce (device)
  - Indicate to the server the device that is connected
  - Request to be added to the queue
- 3: Connection Response (server)
  - Acknowledge connection
  - Either add to the queue or indicate a better time to retry
- 4: Ready (server)
  - Indicate to the device that it is ready and waiting for the authentication process to begin
- 5: Tacho to Card (Device)
- 6: Card to Tacho (Server)
- 7: Abort

Each message type has it's own internal structure

### 0: Ping

Sent to ensure the socket doesn't close prematurely. Either side of the connection can initiate a ping-pong.

Body: no data

### 1: Pong

Sent in response to a ping

Body: no data

### 2: Connection Announce

Device IMEI?
Vehicle reg?
VIN?

### 3: Connection Response

1 byte response code

- 0: Reject
  - Used when the IMEI is to be ignored for whatever reason. Device should use an exponential backoff
- 1: Accept, added to the queue
- 2: Try again later

2 byte retry time (optional)

- Seconds to wait before retrying
- Server uses the recent auth process durations, number of devices in the queue, number of cards in the readers, and number of devices recently turned away to estimate a better time to ask again.

### 4: Ready

???

### 5: Tacho to Card

Raw payload? More info?

### 6: Card to Tacho

Raw payload? More info?

### 7: Abort

1 byte reason:

- 0: Unknown
- 1: Protocol violation
- 2: Card removed
- 3: Card protocol mismatch
- 4: Download too large

From the server that's the whole payload. From a device on firmware G7.0.82 or later two
more bytes follow:

- Stage, the VU exchange in progress: 0 none (not a VU failure, e.g. a malformed
  message from the server), 1 session open, 2 card ready, 3 card authentication,
  4 download request, 5 upload request, 6 transfer data, 7 activities.
- UDS status that exchange got: 0 OK, 1 timeout, 2 unexpected response, 3 busy,
  4 response pending, 5 session lost, 6 rejected.

From firmware H7.0.82 a fourth byte follows: the VU's raw negative response code
(ISO 14229, e.g. 0x24 requestSequenceError, 0x73 wrongBlockSequenceCounter).

The device resets after aborting, so a download it was handing over isn't kept to
resume: the server records it as incomplete.

## Resuming a download

If the connection drops while the device is handing over a download, the server keeps
what it received for 15 minutes. The device keeps its VU session, reconnects and sends
Connection Announce with one extra byte after the firmware version: 1 = resuming a
download (absent or 0 = not).

- **Accepted (1):** no Ready follows and no card is involved. The device carries on
  sending Download Chunks, which the server adds to what it already has, then
  Download Complete with the CRC32 of the whole file as usual. A chunk lost in the
  drop shows up as a CRC mismatch, and the Download Ack tells the device to download
  again.
- **Try again later (2):** there's nothing to resume (the server restarted, more than
  15 minutes passed, or the download was already finished or replaced). The device
  gives up the resume and starts over after the retry time.
- **Reject (0):** the IMEI isn't registered, as for any announce.

A partial download that isn't resumed, or that the device replaces with a new Download
Start, is recorded as incomplete.
