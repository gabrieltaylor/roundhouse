module Ledger
  class Invoice < ApplicationRecord
    self.table_name = "documents"
    self.primary_key = "code"

    has_many :entries, class_name: "Entry", foreign_key: :document_code
    has_many :payers, through: :entries, source: :payable, source_type: "Ledger::Payment"
  end
end
