#include <Wire.h>

#define MPU6050_ADDR 0x68   // AD0 pin low -> 0x68, high -> 0x69

int16_t ax, ay, az;         // accelerometer raw
int16_t gx, gy, gz;         // gyroscope raw
int16_t tempRaw;            // temperature raw

void setup() {
  Serial.begin(9600);
  Wire.begin();             // SDA = A4, SCL = A5 on Pro Mini
  Wire.setClock(400000);    // fast-mode I2C (optional)

  // Wake up the MPU6050 (it powers up in sleep mode)
  Wire.beginTransmission(MPU6050_ADDR);
  Wire.write(0x6B);         // PWR_MGMT_1
  Wire.write(0x00);         // clear sleep bit
  if (Wire.endTransmission(true) != 0) {
    Serial.println("MPU6050 not responding - check wiring!");
    while (1);              // halt
  }

  // Default ranges: accel ±2g, gyro ±250°/s (no config needed)
  delay(100);
  Serial.println("MPU6050 raw data:");
}

void loop() {
  // Burst-read 14 bytes starting at ACCEL_XOUT_H (0x3B)
  Wire.beginTransmission(MPU6050_ADDR);
  Wire.write(0x3B);
  Wire.endTransmission(false);          // repeated start
  Wire.requestFrom(MPU6050_ADDR, 14, true);

  if (Wire.available() == 14) {
    ax = (Wire.read() << 8) | Wire.read();
    ay = (Wire.read() << 8) | Wire.read();
    az = (Wire.read() << 8) | Wire.read();
    tempRaw = (Wire.read() << 8) | Wire.read();
    gx = (Wire.read() << 8) | Wire.read();
    gy = (Wire.read() << 8) | Wire.read();
    gz = (Wire.read() << 8) | Wire.read();

    Serial.print("aX:"); Serial.print(ax);
    Serial.print("\taY:"); Serial.print(ay);
    Serial.print("\taZ:"); Serial.print(az);

    Serial.print("\t| gX:"); Serial.print(gx);
    Serial.print("\tgY:"); Serial.print(gy);
    Serial.print("\tgZ:"); Serial.print(gz);

    Serial.print("\t| T:");  Serial.print(tempRaw);  // raw temp
    // Real temperature = tempRaw / 340.0 + 36.53 (°C) if you want it
    Serial.println();
  }

  delay(100);   // ~10 Hz printout
}