// SPDX-FileCopyrightText: 2026 amurcanov
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

package com.csqtt.client.ui

import java.io.IOException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class ServerArchitectureTest {
    @Test
    fun `uname output maps to the embedded server asset`() {
        assertEquals(ServerArchitecture.AMD64, serverArchitectureForMachine("x86_64\n"))
        assertEquals(ServerArchitecture.ARM64, serverArchitectureForMachine("  aarch64  "))
        assertEquals(ServerArchitecture.ARM64, serverArchitectureForMachine("ARM64"))
        assertEquals(ServerArchitecture.ARMV7, serverArchitectureForMachine("armv7l"))
        assertEquals("csqtt-linux-armv7", ServerArchitecture.ARMV7.assetName)
    }

    @Test
    fun `stderr noise before the machine line is ignored`() {
        assertEquals(ServerArchitecture.AMD64, serverArchitectureForMachine("\n\nx86_64\n"))
        assertEquals(
            ServerArchitecture.ARM64,
            serverArchitectureForMachine("bash: warning: setlocale: LC_ALL: cannot change locale (ru_RU.UTF-8)\naarch64\n"),
        )
    }

    @Test
    fun `unknown or empty machine is rejected`() {
        assertThrows(IOException::class.java) { serverArchitectureForMachine("riscv64") }
        assertThrows(IOException::class.java) { serverArchitectureForMachine("   \n") }
    }
}
